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
# (json 2's JSON.load passes create_additions, so it always takes the
# fallback).
#
# The drop-in follows whichever json is installed, 2.x or 3.x: its
# calling conventions (json 3's keyword-only parse, its dump defaults)
# and its semantics. NOSJ implements json 3's (duplicate keys and lone
# surrogates are errors), so whenever the fast path refuses a call, the
# original gem runs it again and has the last word: json 2 accepts what
# it always accepted, and every exception is the gem's own, message,
# json_path and invalid_object included. That second pass happens on
# failures only; a generate run twice this way calls to_json on the
# objects visited before the refusal twice.
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
    # json 3 made parse's options keyword-only, fixed dump's defaults and
    # raises for options json 2 ignored or aliased.
    JSON3 = ::JSON::VERSION.to_i >= 3

    PARSE_OPTS = %i[symbolize_names freeze max_nesting allow_nan
      allow_trailing_comma allow_duplicate_key].freeze
    # json 2 ignores quirks_mode, which Rails 7.x passes from
    # ActiveSupport::JSON.decode: the fast path drops it (NOSJ.parse
    # always parses top-level scalars). json 3 raises for it, so there it
    # reaches the gem like any other unknown option.
    QUIRKS_MODE = :quirks_mode
    JSON2_PARSE_OPTS = (PARSE_OPTS + [QUIRKS_MODE]).freeze
    GENERATE_OPTS = %i[indent space space_before object_nl array_nl
      max_nesting allow_nan ascii_only script_safe strict depth
      buffer_initial_length allow_duplicate_key].freeze
    JSON3_DUMP_DEFAULTS = {allow_nan: true}.freeze
    # json 2.10 and older accept only strict: in dump's options hash,
    # through this private helper; later versions merge any option.
    DUMP_MERGES_OPTIONS = !::JSON.respond_to?(:merge_dump_options, true)

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
      input = source
      if source.is_a?(String)
        case source.encoding
        when Encoding::UTF_8, Encoding::US_ASCII
        # the fast path as-is
        when Encoding::BINARY
          utf8 = source.dup.force_encoding(Encoding::UTF_8)
          input = utf8 if utf8.valid_encoding?
        else
          return original_parse(source, opts)
        end
      end
      NOSJ.parse(input, opts&.key?(QUIRKS_MODE) ? opts.except(QUIRKS_MODE) : opts)
    rescue NOSJ::ParserError, NOSJ::NestingError
      original_parse(source, opts)
    end

    # The installed gem's parse. json 3 takes keywords only; json 2's
    # positional options hash receives them just the same.
    def original_parse(source, opts)
      ::JSON.nosj_original_parse(source, **(opts || {}))
    end

    def generate(obj, opts, pretty)
      pretty ? NOSJ.pretty_generate(obj, opts) : NOSJ.generate(obj, opts)
    rescue NOSJ::GeneratorError, NOSJ::NestingError
      if pretty
        ::JSON.nosj_original_pretty_generate(obj, opts)
      else
        ::JSON.nosj_original_generate(obj, opts)
      end
    end

    # The options JSON.dump generates with before the caller's own: fixed
    # in json 3; json 2 reads its user-settable dump_default_options,
    # through the internal reader newer 2.x versions added when they
    # deprecated the public one.
    def dump_defaults
      if JSON3
        JSON3_DUMP_DEFAULTS
      elsif ::JSON.respond_to?(:_dump_default_options)
        ::JSON._dump_default_options
      else
        ::JSON.dump_default_options
      end
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

      if NOSJ::JSONDropIn::JSON3
        def parse(source, **opts)
          if NOSJ::JSONDropIn.supported?(opts, NOSJ::JSONDropIn::PARSE_OPTS)
            NOSJ::JSONDropIn.parse(source, opts)
          else
            nosj_original_parse(source, **opts)
          end
        end
      else
        def parse(source, opts = nil)
          if NOSJ::JSONDropIn.supported?(opts, NOSJ::JSONDropIn::JSON2_PARSE_OPTS)
            NOSJ::JSONDropIn.parse(source, opts)
          else
            nosj_original_parse(source, opts)
          end
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
        # hash): the installed gem's dump defaults merged under the
        # caller's options. IO and limit arguments, and anything the
        # fast path refuses, take gem json's own dump, which also turns
        # json 2's NestingError into its ArgumentError.
        if limit.nil? && kwargs.nil? &&
            (an_io.nil? || NOSJ::JSONDropIn::DUMP_MERGES_OPTIONS && an_io.instance_of?(Hash))
          opts = NOSJ::JSONDropIn.dump_defaults
          opts = opts.merge(an_io) if an_io
          if NOSJ::JSONDropIn.supported?(opts, NOSJ::JSONDropIn::GENERATE_OPTS)
            begin
              return NOSJ.generate(obj, opts)
            rescue NOSJ::GeneratorError, NOSJ::NestingError
              # the gem's own dump below runs it again and decides
            end
          end
        end
        nosj_original_dump(obj, an_io, limit, kwargs)
      end
    end
  end
end
