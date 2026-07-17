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

  # Raised when an RFC 6902 patch cannot be applied: a +test+ operation
  # failed, a target or source path does not exist, an array index is
  # out of range, or a +move+ targets its own child. Structurally
  # malformed patch documents (not an op Hash, unknown op, missing
  # fields) raise ArgumentError instead.
  class PatchError < Error; end

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

  # Minifies a document without building any Ruby values: the parser's
  # events pipe straight into the emission kernels, SIMD in and SIMD
  # out. Output is exactly what <code>generate(parse(json))</code>
  # would produce, except duplicate object keys pass through instead of
  # being collapsed (a reformatter must not silently drop data).
  # Numbers come out in the canonical spelling (+1.50+ becomes +1.5+)
  # and string escapes are normalized.
  #
  # @example
  #   NOSJ.minify(%({ "a": [1, 2],\n  "b": "x" }))  #=> '{"a":[1,2],"b":"x"}'
  #
  # @param json [String] the document (UTF-8 or US-ASCII)
  # @param opts [Hash, nil] acceptance options (+allow_nan+,
  #   +allow_trailing_comma+, +max_nesting+); trailing commas are
  #   normalized away when accepted
  # @return [String] the minified document
  # @raise [ParserError] when the document is malformed
  # @raise [NestingError] past +max_nesting+
  def self.minify(json, opts = nil)
    reformat_native(json, opts)
  end

  # Reformats a document without building any Ruby values: {.minify}'s
  # pipe with formatting. <code>pretty: true</code> is a shorthand for
  # {.pretty_generate}'s layout; the individual {.generate} formatting
  # and escape options (+indent+, +space+, +object_nl+, +ascii_only+,
  # +script_safe+, ...) compose with it and win over it.
  #
  # @example
  #   NOSJ.reformat(json, pretty: true)
  #   NOSJ.reformat(json, ascii_only: true)   # escape-transcode, compact
  #
  # @param json [String] the document (UTF-8 or US-ASCII)
  # @param opts [Hash, nil] +pretty+, {.generate} formatting/escape
  #   options, and {.minify}'s acceptance options
  # @return [String] the reformatted document
  # @raise [ParserError] when the document is malformed
  # @raise [NestingError] past +max_nesting+
  # @raise [GeneratorError] when +ascii_only+ meets a lone-surrogate
  #   string it cannot represent
  def self.reformat(json, opts = nil)
    if opts&.key?(:pretty)
      pretty = opts[:pretty]
      opts = opts.except(:pretty)
      opts = PRETTY_GENERATE_OPTS.merge(opts) if pretty
    end
    reformat_native(json, opts)
  end

  # {.reformat} against a file: the pipe runs over a read-only memory
  # map, so the input document never becomes a Ruby String; only the
  # result does.
  #
  # @example
  #   compact = NOSJ.reformat_file("big.json")            # minify
  #   pretty  = NOSJ.reformat_file("big.json", pretty: true)
  #
  # @param path [String] the file to reformat (UTF-8)
  # @param opts [Hash, nil] same options as {.reformat}
  # @return [String] the reformatted document
  # @raise [SystemCallError] +Errno::ENOENT+ and friends
  # @raise [ParserError] when the file is malformed or not UTF-8
  def self.reformat_file(path, opts = nil)
    if opts&.key?(:pretty)
      pretty = opts[:pretty]
      opts = opts.except(:pretty)
      opts = PRETTY_GENERATE_OPTS.merge(opts) if pretty
    end
    reformat_file_native(path, opts)
  end

  # Byte-splicing edits: replaces the values at the given JSON Pointers
  # directly in the text. Every target resolves in ONE forward pass
  # (skipping, not parsing), and the result is built in one sweep:
  # every byte outside the target spans is copied untouched, so
  # formatting, key order, and number spellings elsewhere are preserved
  # exactly. For tweaking a field in a passing payload this replaces
  # the whole parse → mutate → generate cycle.
  #
  # @example
  #   NOSJ.splice(json, "/config/timeout" => 30)
  #   NOSJ.splice(json, "/a" => 1, "/b/c" => [true])   # batch, one pass
  #
  # @param json [String] the document (UTF-8 or US-ASCII)
  # @param edits [Hash{String => Object}] JSON Pointer => replacement
  #   value (generated compactly, byte-identical to {.generate})
  # @param opts [Hash, nil] {.generate} options for the inserted values
  # @return [String] the edited document
  # @raise [KeyError] when a pointer does not resolve (splice replaces;
  #   use {.patch} +add+ to insert)
  # @raise [ArgumentError] for malformed pointers or overlapping targets
  # @raise [ParserError] when the document is malformed
  def self.splice(json, edits, opts = nil)
    splice_native(json, edits, opts)
  end

  # Applies an RFC 6902 JSON Patch to the raw document: +add+,
  # +remove+, +replace+, +move+, +copy+, and +test+, applied
  # sequentially, each as a byte-splice (structural ops walk only the
  # parent container's span). Op hashes accept String or Symbol keys.
  #
  # @example
  #   NOSJ.patch(json, [
  #     {"op" => "test", "path" => "/a", "value" => 1},
  #     {"op" => "replace", "path" => "/a", "value" => 2},
  #     {"op" => "add", "path" => "/list/-", "value" => "x"},
  #     {"op" => "move", "from" => "/tmp", "path" => "/kept"}
  #   ])
  #
  # @param json [String] the document (UTF-8 or US-ASCII)
  # @param ops [Array<Hash>] RFC 6902 operations
  # @param opts [Hash, nil] {.generate} options for inserted values
  # @return [String] the patched document
  # @raise [PatchError] when application fails (failed +test+, missing
  #   target, index out of range, move into own child)
  # @raise [ArgumentError] for structurally malformed patch documents
  # @raise [ParserError] when the document is malformed
  def self.patch(json, ops, opts = nil)
    patch_native(json, ops, opts)
  end

  # Applies an RFC 7386 JSON Merge Patch: +nil+ values remove keys,
  # nested Hashes merge recursively, everything else replaces. This is
  # the semantic form (parse, merge, generate); Symbol keys in +patch+
  # match String keys in the document.
  #
  # @example
  #   NOSJ.merge_patch(%({"a":{"b":1,"c":2}}), {a: {b: nil, d: 3}})
  #   #=> '{"a":{"c":2,"d":3}}'
  #
  # @param json [String] the document (UTF-8 or US-ASCII)
  # @param patch [Object] the merge patch (a non-Hash replaces the
  #   whole document)
  # @param opts [Hash, nil] {.generate} options for the result
  # @return [String] the merged document
  # @raise [ParserError] when the document is malformed
  def self.merge_patch(json, patch, opts = nil)
    return generate(patch, opts) unless patch.is_a?(Hash)
    generate(merge_patch_value(parse(json), patch), opts)
  end

  # RFC 7386, applied to parsed values.
  def self.merge_patch_value(target, patch)
    return patch unless patch.is_a?(Hash)
    target = {} unless target.is_a?(Hash)
    out = target.dup
    patch.each do |key, value|
      key = key.to_s
      if value.nil?
        out.delete(key)
      else
        out[key] = merge_patch_value(out[key], value)
      end
    end
    out
  end
  private_class_method :merge_patch_value

  # NDJSON / JSON Lines: yields one parsed value per line of +source+.
  # Framing is exact because a raw newline can never occur inside a
  # JSON value; blank lines are skipped (the NDJSON convention). One
  # value per line is enforced: a second value on a line raises, and a
  # malformed line raises {ParserError} whose {ParserError#line} is the
  # physical line number in +source+.
  #
  # Pass a frozen string for zero-copy iteration (an unfrozen source is
  # walked over a private copy, like {.lazy}). The block may itself
  # call any NOSJ method.
  #
  # @example
  #   NOSJ.each_line(log) { |event| ingest(event) }
  #   NOSJ.each_line(log).first(10)          # Enumerator when blockless
  #
  # @param source [String] newline-delimited JSON (UTF-8 or US-ASCII)
  # @param opts [Hash, nil] the same options as {.parse}, applied per line
  # @yieldparam value [Object] one parsed document per non-blank line
  # @return [Enumerator] when no block is given, else +nil+
  # @raise [ParserError] on the first malformed line
  def self.each_line(source, opts = nil, &block)
    return enum_for(:each_line, source, opts) unless block
    each_line_native(source, opts, &block)
  end

  # {.each_line} against a file: the NDJSON stream is walked over a
  # read-only memory map, so the file never becomes a Ruby String.
  #
  # @example
  #   NOSJ.each_line_file("events.ndjson") { |event| ingest(event) }
  #
  # @param path [String] the NDJSON file (UTF-8)
  # @param opts [Hash, nil] the same options as {.parse}, applied per line
  # @yieldparam value [Object] one parsed document per non-blank line
  # @return [Enumerator] when no block is given, else +nil+
  # @raise [SystemCallError] +Errno::ENOENT+ and friends
  # @raise [ParserError] on the first malformed line
  def self.each_line_file(path, opts = nil, &block)
    return enum_for(:each_line_file, path, opts) unless block
    each_line_file_native(path, opts, &block)
  end

  # Generates NDJSON / JSON Lines: one compact document per element,
  # each terminated with a newline, built in a single pass into one
  # buffer. Formatting options containing newlines raise ArgumentError
  # (they would break the line framing); everything else from
  # {.generate} applies.
  #
  # @example
  #   NOSJ.generate_lines([{a: 1}, {b: 2}])  #=> %({"a":1}\n{"b":2}\n)
  #
  # @param values [Array, Enumerable] one document per element
  # @param opts [Hash, nil] the same options as {.generate}
  # @return [String] the NDJSON document (empty when +values+ is empty)
  # @raise [ArgumentError] for formatting options that contain newlines
  # @raise [GeneratorError] (see {.generate})
  def self.generate_lines(values, opts = nil)
    generate_lines_native(lines_array(values), opts)
  end

  # {.generate_lines} straight to a file, streaming the generator's
  # buffer to disk like {.write_file}.
  #
  # @example
  #   NOSJ.write_lines("out.ndjson", events)  #=> bytes written
  #
  # @param path [String] the file to (over)write
  # @param values [Array, Enumerable] one document per line
  # @param opts [Hash, nil] the same options as {.generate}
  # @return [Integer] the number of bytes written, like File.write
  # @raise [SystemCallError] +Errno::ENOENT+ and friends
  # @raise [ArgumentError] for formatting options that contain newlines
  # @raise [GeneratorError] (see {.generate})
  def self.write_lines(path, values, opts = nil)
    write_lines_native(path, lines_array(values), opts)
  end

  # One document per element: Arrays pass through, other Enumerables
  # convert, anything else (nil included, whose to_a would silently
  # yield []) is a TypeError.
  def self.lines_array(values)
    return values if values.is_a?(Array)
    unless values.is_a?(Enumerable)
      raise TypeError, "no implicit conversion of #{values.class} into Array"
    end
    values.to_a
  end
  private_class_method :lines_array

  # Document statistics from one full-parser pass into a counting sink:
  # no Ruby value is built for the document itself, only the small
  # result Hash. A debugging endpoint for "what is this 40 MB blob".
  #
  # The result (Symbol keys):
  #
  #   {
  #     byte_size: 631,             # of the document
  #     root: :object,              # :object/:array/:string/:integer/
  #                                 # :float/:boolean/:null
  #     max_depth: 4,               # container nesting, counted like
  #                                 # max_nesting (root container = 1)
  #     values: {total:, objects:, arrays:, strings:, integers:,
  #              floats:, booleans:, nulls:},
  #     keys: {total:, unique:},
  #     key_histogram: {"name" => 128, ...},  # sorted by count desc,
  #                                           # so .first(10) = top 10
  #     containers: {max_object_entries:, max_array_length:},
  #     strings: {bytes:, max_bytes:}         # decoded UTF-8 bytes
  #   }
  #
  # Unlike {.parse}, nesting is UNLIMITED by default (a deep blob is
  # exactly what a diagnostic should describe); pass +max_nesting+ to
  # enforce a limit. Histogram memory is proportional to the number of
  # unique keys.
  #
  # @example Top ten keys of a mystery blob
  #   NOSJ.stats(blob)[:key_histogram].first(10)
  #
  # @param source [String] the JSON document (UTF-8 or US-ASCII)
  # @param opts [Hash, nil] +max_nesting+, +allow_nan+,
  #   +allow_trailing_comma+ (acceptance options only)
  # @return [Hash] the statistics described above
  # @raise [ParserError] when the document is malformed or not UTF-8
  def self.stats(source, opts = nil)
    stats_native(source, opts)
  end

  # {.stats} against a file: memory-maps it and runs the counting pass
  # without reading the document into Ruby. +byte_size+ is the file
  # size.
  #
  # @example
  #   NOSJ.stats_file("huge.json") => {byte_size: 41_943_040, ...}
  #
  # @param path [String] the file to inspect (UTF-8)
  # @param opts [Hash, nil] same options as {.stats}
  # @return [Hash] the statistics described on {.stats}
  # @raise [SystemCallError] +Errno::ENOENT+ and friends
  # @raise [ParserError] when the file is malformed or not UTF-8
  def self.stats_file(path, opts = nil)
    stats_file_native(path, opts)
  end
end
