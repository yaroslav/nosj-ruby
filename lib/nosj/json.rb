# frozen_string_literal: true

# Drop-in acceleration for the JSON module:
#
#   require "nosj/json"
#
# reroutes JSON.parse, JSON.generate, JSON.pretty_generate and JSON.dump
# through NOSJ whenever the requested options fall within NOSJ's
# supported set, and falls back to gem json's own implementation for
# everything else (create_additions, object_class/array_class,
# decimal_class, on_load procs, JSON::State instances, IO arguments).
# Entry points built on JSON.parse (JSON.load, JSON.parse!,
# JSON.load_file, JSON.unsafe_load) pick up the fast path automatically
# and keep their exact legacy behavior when they need unsupported options
# (JSON.load's create_additions default always takes the fallback).
#
# Exceptions from the fast path are re-raised as the JSON classes
# (JSON::ParserError, JSON::GeneratorError, JSON::NestingError), so
# existing rescue clauses keep working. Parse error MESSAGES are
# NOSJ's (byte offsets rather than the gem's phrasing).
#
# Not rerouted: obj.to_json (core extensions drive the gem's generator
# directly), and objects with a custom to_json inside a rerouted
# generate receive no State argument (documented NOSJ divergence).

require "json"
require "nosj"

module NOSJ
  # Implementation detail of `require "nosj/json"`.
  # @private
  module JSONDropIn
    # quirks_mode rides the fast path because NOSJ.parse is always
    # quirks-mode (top-level scalars parse) and ignores the key; Rails
    # 7.x passes it from ActiveSupport::JSON.decode.
    PARSE_OPTS = %i[symbolize_names freeze max_nesting allow_nan
      allow_trailing_comma quirks_mode].freeze
    GENERATE_OPTS = %i[indent space space_before object_nl array_nl
      max_nesting allow_nan ascii_only script_safe
      escape_slash strict depth
      buffer_initial_length].freeze

    module_function

    # The fast path handles nil or a plain Hash whose every key NOSJ
    # implements; anything else (JSON::State, exotic options, string
    # keys) belongs to gem json.
    def supported?(opts, allowed)
      return true if opts.nil?
      return false unless opts.instance_of?(Hash)
      opts.each_key { |k| return false unless allowed.include?(k) }
      true
    end

    def parse(source, opts)
      # NOSJ.parse is deliberately strict about encodings (json-3.0
      # semantics), but the drop-in must match the installed gem, which
      # accepts more. BINARY strings holding valid UTF-8 are the big
      # real-world case: Rack delivers request bodies as BINARY, so
      # Rails JSON params come through here. Retagging a dup is cheap
      # (copy-on-write bytes), and the validity scan is memoized
      # coderange the parse would compute anyway. Anything else
      # non-UTF-8 (UTF-16, ...) belongs to gem json, which transcodes.
      if source.is_a?(String)
        case source.encoding
        when Encoding::UTF_8, Encoding::US_ASCII
        # the fast path as-is
        when Encoding::BINARY
          utf8 = source.dup.force_encoding(Encoding::UTF_8)
          source = utf8 if utf8.valid_encoding?
        else
          return ::JSON.nosj_original_parse(source, **(opts || {}))
        end
      end
      NOSJ.parse(source, opts)
    rescue RuntimeError => e
      raise ::JSON::ParserError, e.message
    end

    def generate(obj, opts, pretty)
      pretty ? NOSJ.pretty_generate(obj, opts) : NOSJ.generate(obj, opts)
    rescue NOSJ::NestingError => e
      raise ::JSON::NestingError, e.message
    rescue NOSJ::GeneratorError => e
      raise ::JSON::GeneratorError, e.message
    end
  end
end

# Reopened by `require "nosj/json"` to reroute the module functions
# through NOSJ; behavior is documented on the require and in the
# README, not here.
# @private
module JSON
  class << self
    unless method_defined?(:nosj_original_parse) || private_method_defined?(:nosj_original_parse)
      alias_method :nosj_original_parse, :parse
      alias_method :nosj_original_generate, :generate
      alias_method :nosj_original_pretty_generate, :pretty_generate
      alias_method :nosj_original_dump, :dump

      def parse(source, opts = nil)
        if NOSJ::JSONDropIn.supported?(opts, NOSJ::JSONDropIn::PARSE_OPTS)
          NOSJ::JSONDropIn.parse(source, opts)
        else
          nosj_original_parse(source, opts)
        end
      end

      def generate(obj, opts = nil)
        if NOSJ::JSONDropIn.supported?(opts, NOSJ::JSONDropIn::GENERATE_OPTS)
          NOSJ::JSONDropIn.generate(obj, opts, false)
        else
          nosj_original_generate(obj, opts)
        end
      end

      def pretty_generate(obj, opts = nil)
        if NOSJ::JSONDropIn.supported?(opts, NOSJ::JSONDropIn::GENERATE_OPTS)
          NOSJ::JSONDropIn.generate(obj, opts, true)
        else
          nosj_original_pretty_generate(obj, opts)
        end
      end

      def dump(obj, an_io = nil, limit = nil, kwargs = nil)
        # Fast path for the common shapes, dump(obj) and dump(obj, opts
        # hash), mirroring gem json: dump defaults merged under the
        # user's options, NestingError surfaced as ArgumentError. IO and
        # limit arguments take gem json's own dump.
        if limit.nil? && kwargs.nil? && (an_io.nil? || an_io.instance_of?(Hash))
          opts = _dump_default_options
          opts = opts.merge(an_io) if an_io
          if NOSJ::JSONDropIn.supported?(opts, NOSJ::JSONDropIn::GENERATE_OPTS)
            begin
              return NOSJ::JSONDropIn.generate(obj, opts, false)
            rescue ::JSON::NestingError
              raise ArgumentError, "exceed depth limit"
            end
          end
        end
        nosj_original_dump(obj, an_io, limit, kwargs)
      end
    end
  end
end
