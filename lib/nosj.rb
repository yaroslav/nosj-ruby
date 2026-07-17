# frozen_string_literal: true

require_relative "nosj/version"
require_relative "nosj/native"
require_relative "nosj/lazy"

# nosj is the evil twin of the +json+ gem: the same API, output bytes,
# option names, and error messages, backed by the Rust
# {https://github.com/yaroslav/nosj nosj} crate with SIMD-accelerated
# parsing and generation. Beyond the +json+ gem's surface it adds
# zero-allocation validation ({.valid?}), partial parsing
# ({.dig}, {.at_pointer}, and their batch forms), and lazy documents
# ({.lazy}).
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

  # Raised when a document cannot be parsed. Carries the failure
  # position, computed once when the parse fails (successful parses
  # never pay for it): {#byte_offset}, 1-based {#line}, character-based
  # {#column}, and a caret {#snippet} pointing at the offending spot.
  # Positions are absolute within the document you passed, including
  # through partial parsing ({NOSJ.dig}, {NOSJ.at_pointer}, lazy
  # documents) and the file APIs. All four are +nil+ for failures
  # without a position (encoding refusals).
  #
  # @example
  #   NOSJ.parse(%({\n  "a": 1,\n  "b": }))
  #   # => NOSJ::ParserError, with
  #   #    e.line     #=> 3
  #   #    e.column   #=> 8
  #   #    e.snippet  #=> "  \"b\": }\n       ^"
  class ParserError < Error
    # @return [Integer, nil] byte offset of the failure in the source
    attr_reader :byte_offset
    # @return [Integer, nil] 1-based line of the failure
    attr_reader :line
    # @return [Integer, nil] 1-based character (not byte) column within
    #   {#line}
    attr_reader :column
    # @return [String, nil] the offending line (windowed when long)
    #   with a caret line underneath
    attr_reader :snippet

    # The default message plus {#snippet}: Ruby prints
    # +detailed_message+ when an exception reaches the top level, so an
    # unrescued parse error shows where the document broke.
    def detailed_message(highlight: false, **opts)
      base = super
      snippet ? "#{base}\n#{snippet}" : base
    end
  end

  # Raised when a value cannot be generated (non-finite floats without
  # +allow_nan+, unsupported objects under +strict+, broken encodings).
  # Message-compatible with +JSON::GeneratorError+.
  class GeneratorError < Error; end

  # Raised when parsing or generation exceeds +max_nesting+.
  # Message-compatible with +JSON::NestingError+.
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
  # @raise [ParserError] when the document is malformed or not UTF-8;
  #   carries the failure position ({ParserError#line} and friends)
  # @raise [NestingError] when nesting exceeds +max_nesting+
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

  # Parses a JSON file, like +JSON.load_file+—except the file is read
  # natively into a reused buffer, so no file-sized Ruby String is ever
  # created (or garbage-collected).
  #
  # @example
  #   NOSJ.load_file("config.json", symbolize_names: true)
  #
  # @param path [String] the file to parse (UTF-8)
  # @param opts [Hash, nil] the same options as {.parse}
  # @return [Object] the parsed value tree
  # @raise [SystemCallError] +Errno::ENOENT+ and friends, like File.read
  # @raise [ParserError] when the document is malformed or not UTF-8
  def self.load_file(path, opts = nil)
    load_file_native(path, opts)
  end

  # Generates +obj+ as JSON and writes it to +path+, streaming the
  # generator's buffer straight to disk—no intermediate Ruby String.
  #
  # @example
  #   NOSJ.write_file("out.json", {"a" => [1, true]})   #=> 14
  #   NOSJ.write_file("pretty.json", obj, indent: "  ", object_nl: "\n")
  #
  # @param path [String] the file to (over)write
  # @param obj [Object] the value tree to generate
  # @param opts [Hash, nil] the same options as {.generate}
  # @return [Integer] the number of bytes written, like File.write
  # @raise [SystemCallError] +Errno::ENOENT+ and friends, like File.write
  # @raise [GeneratorError] like {.generate}
  def self.write_file(path, obj, opts = nil)
    write_file_native(path, obj, opts)
  end

  # Wraps a JSON file as a lazy document ({.lazy} for files): the file
  # is memory-mapped read-only, so beyond one sequential UTF-8 check,
  # pages you never read are never loaded from disk. The mapping lives
  # as long as any node on it; the file must not be modified while it
  # is in use.
  #
  # @example
  #   doc = NOSJ.load_lazy_file("huge.json")
  #   doc["users"][3]["name"]   # touches only these pages
  #
  # @param path [String] the file to wrap (UTF-8)
  # @param opts [Hash, nil] {.parse} options applied on materialization
  # @return [NOSJ::Lazy, Object]
  # @raise [SystemCallError] +Errno::ENOENT+ and friends
  # @raise [ParserError] when the file is not UTF-8 or the root is malformed
  def self.load_lazy_file(path, opts = nil)
    load_lazy_file_native(path, opts)
  end

  # {.at_pointer} against a file: memory-maps it, resolves the pointer,
  # materializes only the matched subtree, and never reads the rest
  # into Ruby.
  #
  # @example
  #   NOSJ.at_pointer_file("huge.json", "/users/3/name")
  #
  # @param path [String] the file to query (UTF-8)
  # @param pointer [String] an RFC 6901 JSON Pointer
  # @param opts [Hash, nil] materialization options
  # @return [Object, nil] nil when the pointer misses
  # @raise [ArgumentError] for malformed pointers
  def self.at_pointer_file(path, pointer, opts = nil)
    at_pointer_file_native(path, pointer, opts)
  end

  # {.dig} against a file: the Hash#dig-shaped counterpart of
  # {.at_pointer_file}. Negative indices resolve to nil.
  #
  # @example
  #   NOSJ.dig_file("huge.json", "users", 3, "name")
  #
  # @param path [String] the file to query (UTF-8)
  # @param path_elements [Array<String, Symbol, Integer>]
  # @return [Object, nil]
  def self.dig_file(path, *path_elements)
    dig_file_native(path, path_elements)
  end
end
