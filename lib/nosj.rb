# frozen_string_literal: true

require_relative "nosj/version"
require_relative "nosj/native"

# nosj is the evil twin of the +json+ gem: the same API, output bytes,
# option names, and error messages, backed by the Rust
# {https://github.com/yaroslav/nosj nosj} crate with SIMD-accelerated
# parsing and generation. Beyond the +json+ gem's surface it adds
# zero-allocation validation ({.valid?}) and partial parsing
# ({.dig}, {.at_pointer}, and their batch forms).
#
# Options arrive as a positional Hash (the +json+ gem's own calling
# convention); an explicit +**kwargs+ would allocate per call.
#
# @example The json gem API
#   NOSJ.parse('{"a":[1,true]}')        #=> {"a" => [1, true]}
#   NOSJ.generate({"a" => [1, true]})   #=> '{"a":[1,true]}'
#
# @example Drop-in acceleration for the JSON module
#   require "nosj/json"
#   JSON.parse(src)  # routed through nosj
module NOSJ
  # Base class for nosj errors.
  class Error < StandardError; end

  # Raised when a value cannot be generated (non-finite floats without
  # +allow_nan+, unsupported objects under +strict+, broken encodings).
  # Message-compatible with +JSON::GeneratorError+.
  class GeneratorError < Error; end

  # Raised when generation exceeds +max_nesting+. Message-compatible
  # with +JSON::NestingError+.
  class NestingError < Error; end

  PRETTY_GENERATE_OPTS = {
    indent: "  ", space: " ", object_nl: "\n", array_nl: "\n"
  }.freeze
  private_constant :PRETTY_GENERATE_OPTS

  # Parses a JSON document, JSON.parse-compatible: same values, same
  # option names, same behavior, byte-for-byte.
  #
  # The +json+ gem's legacy object-deserialization options
  # (+object_class+, +array_class+, +decimal_class+,
  # +create_additions+) are deliberately unsupported and raise
  # ArgumentError.
  #
  # @example
  #   NOSJ.parse('{"a":[1,true]}')                      #=> {"a" => [1, true]}
  #   NOSJ.parse('{"a":1}', symbolize_names: true)      #=> {a: 1}
  #
  # @param source [String] the JSON document (UTF-8 or US-ASCII)
  # @param opts [Hash, nil] +symbolize_names+, +freeze+, +max_nesting+
  #   (Integer or +false+ for unlimited), +allow_nan+,
  #   +allow_trailing_comma+
  # @return [Object] the parsed value tree
  # @raise [RuntimeError] when the document is malformed or not UTF-8
  # @raise [ArgumentError] for unsupported options
  def self.parse(source, opts = nil)
    parse_native(source, opts)
  end

  # @!method self.generate(obj, opts = nil)
  #   Generates JSON, JSON.generate-compatible: identical output bytes,
  #   including the gem's exact float formatting. Implemented natively
  #   (no Ruby forwarder frame; the definition lives in the extension).
  #
  #   @example
  #     NOSJ.generate({"a" => [1, true]})  #=> '{"a":[1,true]}'
  #
  #   @param obj [Object] the value tree to serialize
  #   @param opts [Hash, nil] +indent+, +space+, +space_before+,
  #     +object_nl+, +array_nl+, +max_nesting+ (Integer or +false+),
  #     +allow_nan+, +ascii_only+, +script_safe+ (alias +escape_slash+),
  #     +strict+, +depth+, +buffer_initial_length+
  #   @return [String] the JSON document
  #   @raise [GeneratorError] for non-finite floats without +allow_nan+,
  #     unsupported objects under +strict+, or broken string encodings
  #   @raise [NestingError] when nesting exceeds +max_nesting+

  # Generates human-readable JSON, JSON.pretty_generate-compatible
  # (two-space indent, newlines between elements). Options override the
  # pretty defaults and are otherwise the same as {.generate}.
  #
  # @param obj [Object] the value tree to serialize
  # @param opts [Hash, nil] see {.generate}
  # @return [String] the pretty-printed JSON document
  # @raise [GeneratorError] (see {.generate})
  # @raise [NestingError] (see {.generate})
  def self.pretty_generate(obj, opts = nil)
    opts = opts.nil? ? PRETTY_GENERATE_OPTS : PRETTY_GENERATE_OPTS.merge(opts)
    generate_native(obj, opts)
  end

  # Validates a document without building any Ruby values: the full
  # parser (tokenizers, string decode, number validation) runs into a
  # null sink, 1.8-2.5x faster than {.parse}.
  #
  # Returns true iff <code>NOSJ.parse(source, opts)</code> would
  # succeed: parse refusals (malformed JSON, wrong encoding, too-deep
  # nesting) are +false+, while option and argument-type errors still
  # raise exactly like {.parse}.
  #
  # @example
  #   NOSJ.valid?('{"a":1}')  #=> true
  #   NOSJ.valid?('{"a":}')   #=> false
  #
  # @param source [String] the JSON document
  # @param opts [Hash, nil] same options as {.parse}
  # @return [Boolean]
  # @raise [ArgumentError] for unsupported options
  def self.valid?(source, opts = nil)
    valid_native(source, opts)
  end

  # Partial parsing, Hash#dig-shaped: extracts one value from a JSON
  # string without materializing the rest of the document. Skipped
  # content is stepped over at SIMD block speed, so a lookup costs what
  # it skips, not what the document weighs.
  #
  # @example
  #   NOSJ.dig(json, "users", 3, "name")  #=> "grace" or nil
  #
  # @param source [String] the JSON document
  # @param path [Array<String, Symbol, Integer>] object keys and array
  #   indices; unlike Array#dig, negative indices return +nil+ (JSON
  #   Pointer has no equivalent)
  # @return [Object, nil] the matched value, or +nil+ when the path
  #   does not resolve
  # @raise [ArgumentError] for path elements that are not Strings,
  #   Symbols, or Integers
  def self.dig(source, *path)
    dig_native(source, path)
  end

  # Batch {.dig}: many paths resolved in ONE pass over the document.
  # The walk descends only into subtrees some path still needs, so a
  # batch costs about as much as its single deepest lookup.
  #
  # On malformed documents a batch may raise where a single dig would
  # return +nil+: one pass scans every byte some path needs.
  #
  # @example
  #   NOSJ.dig_many(json, [["users", 3, "name"], ["meta", "count"]])
  #   #=> ["grace", 42]
  #
  # @param source [String] the JSON document
  # @param paths [Array<Array<String, Symbol, Integer>>] one dig path
  #   per result
  # @param opts [Hash, nil] materialization options ({.parse}'s
  #   +symbolize_names+, +freeze+, ...)
  # @return [Array<Object, nil>] positionally aligned with +paths+
  # @raise [ArgumentError] for malformed paths
  def self.dig_many(source, paths, opts = nil)
    dig_many_native(source, paths, opts)
  end

  # Partial parsing by JSON Pointer (with the standard +~0+/+~1+
  # escapes). The matched subtree materializes under the same options
  # as {.parse}.
  #
  # @example
  #   NOSJ.at_pointer(json, "/users/3/name")  #=> "grace" or nil
  #
  # @param source [String] the JSON document
  # @param pointer [String] a JSON Pointer (empty string = whole
  #   document)
  # @param opts [Hash, nil] materialization options
  # @return [Object, nil] the matched value, or +nil+ when the pointer
  #   does not resolve
  # @raise [ArgumentError] for a malformed pointer (non-empty without a
  #   leading +/+)
  def self.at_pointer(source, pointer, opts = nil)
    at_pointer_native(source, pointer, opts)
  end

  # Batch {.at_pointer}: the pointer-string counterpart of {.dig_many},
  # resolving the whole set in one pass.
  #
  # @example
  #   NOSJ.at_pointers(json, ["/users/3/name", "/meta/count"])
  #   #=> ["grace", 42]
  #
  # @param source [String] the JSON document
  # @param pointers [Array<String>] JSON Pointers, one per result
  # @param opts [Hash, nil] materialization options
  # @return [Array<Object, nil>] positionally aligned with +pointers+
  # @raise [ArgumentError] for malformed pointers
  def self.at_pointers(source, pointers, opts = nil)
    at_pointers_native(source, pointers, opts)
  end
end
