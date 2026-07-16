# frozen_string_literal: true

module NOSJ
  # A lazy view of one JSON container inside a document created with
  # {NOSJ.lazy}. Nothing is parsed until it is touched: indexing walks
  # the raw bytes to the requested child (skipping siblings at SIMD
  # block speed), returns another Lazy node for containers, and
  # materializes plain Ruby values for scalars. Resolved children are
  # cached, so repeated access is free and object identity is stable.
  #
  # Validation is as-you-go: skipped content is bracket-balance
  # checked and resolved targets fully validated, so a malformed region
  # raises when an access first walks it, not at {NOSJ.lazy} time.
  #
  # Nodes hold their own stable copy of the document (shared across the
  # whole node tree), so mutating the source string later is harmless.
  #
  # @example Pull two fields out of a big document
  #   doc = NOSJ.lazy(huge_json)
  #   doc["users"][3]["name"]     # only this path is parsed
  #   doc.dig("meta", "count")
  #
  # @example Materialize a subtree
  #   doc["users"][3].value       #=> {"name" => ..., ...}
  class Lazy
    include Enumerable

    # Resolves one child. String and Symbol keys index objects; Integer
    # indices index arrays (negative indices resolve to +nil+, as in
    # {NOSJ.dig}). Containers come back as further Lazy nodes, scalars
    # as plain Ruby values, misses as +nil+. Results are cached on the
    # node.
    #
    # @param token [String, Symbol, Integer]
    # @return [NOSJ::Lazy, Object, nil]
    def [](token)
      cache = (@children ||= {})
      cache.fetch(token) { cache[token] = __get(token) }
    end

    # Hash#dig-shaped access, fused into a single resolution: the whole
    # path resolves in one walk of this node's bytes, no intermediate
    # nodes. Semantics match {NOSJ.dig}: misses, negative indices, and
    # steps into scalars resolve to +nil+. Not cached.
    #
    # @param path [Array<String, Symbol, Integer>]
    # @return [NOSJ::Lazy, Object, nil]
    def dig(first, *rest)
      __dig([first, *rest])
    end

    # Resolves an RFC 6901 JSON Pointer within this node's subtree,
    # with the same lazy/materialize behavior as {#[]}. Not cached.
    #
    # @param pointer [String] e.g. <tt>"/users/3/name"</tt>
    # @return [NOSJ::Lazy, Object, nil]
    def at_pointer(pointer)
      __at_pointer(pointer)
    end

    # Materializes this node's whole subtree as plain Ruby values,
    # under the options given to {NOSJ.lazy} (+symbolize_names+,
    # +freeze+, ...).
    #
    # @return [Hash, Array]
    def value
      __materialize
    end
    alias_method :materialize, :value

    # @return [Hash] the materialized subtree
    # @raise [TypeError] on an array node
    def to_h
      raise TypeError, "to_h on a JSON array" unless object?

      __materialize
    end

    # @return [Array] the materialized subtree
    # @raise [TypeError] on an object node
    def to_a
      raise TypeError, "to_a on a JSON object" unless array?

      __materialize
    end

    # @return [Boolean] whether this node is a JSON object
    def object?
      __kind == :object
    end

    # @return [Boolean] whether this node is a JSON array
    def array?
      __kind == :array
    end

    # The object's keys (always Strings), read in one walk without
    # materializing any values.
    #
    # @return [Array<String>]
    # @raise [TypeError] on an array node
    def keys
      __keys
    end

    # Entry count (object pairs or array elements), read in one walk
    # without materializing anything.
    #
    # @return [Integer]
    def size
      __size
    end
    alias_method :length, :size
    alias_method :count, :size

    # @return [Boolean]
    def empty?
      size.zero?
    end

    # Iterates direct children, resolved in one walk: object nodes
    # yield +[key, child]+ pairs (like Hash), array nodes yield
    # children. Containers arrive as Lazy nodes, scalars as values.
    #
    # @return [Enumerator] when no block is given
    def each(&block)
      return enum_for(:each) unless block

      if object?
        __children.each { |pair| yield pair }
      else
        __children.each { |child| yield child }
      end
      self
    end

    # @return [String] kind and span size, without touching content
    def inspect
      "#<NOSJ::Lazy #{__kind} (#{__byte_size} bytes)>"
    end
    alias_method :to_s, :inspect

    private :__get, :__dig, :__at_pointer, :__materialize, :__kind,
      :__byte_size, :__keys, :__size, :__children
  end

  # Wraps a JSON document for lazy, on-demand access: returns a
  # {NOSJ::Lazy} node for a container root, or the materialized value
  # for a scalar root. The document bytes are copied once; no parsing
  # happens beyond locating the root value.
  #
  # @example
  #   doc = NOSJ.lazy('{"users":[{"name":"ada"},{"name":"grace"}]}')
  #   doc["users"][1]["name"]  #=> "grace" — the rest is never parsed
  #
  # @param source [String] the JSON document (UTF-8 or US-ASCII)
  # @param opts [Hash, nil] {NOSJ.parse} options applied whenever a
  #   value materializes: +symbolize_names+, +freeze+, +max_nesting+,
  #   +allow_nan+, +allow_trailing_comma+
  # @return [NOSJ::Lazy, Object]
  # @raise [RuntimeError] when the document root is malformed
  def self.lazy(source, opts = nil)
    lazy_native(source, opts)
  end
end
