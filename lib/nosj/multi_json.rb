# frozen_string_literal: true

# MultiJson adapter:
#
#   require "nosj/multi_json"
#   MultiJson.use NOSJ::MultiJsonAdapter
#
# Anything speaking MultiJson (Faraday middleware and friends) then
# parses and generates through NOSJ.

require "json"
require "multi_json"
require "multi_json/adapter"
require "nosj"

module NOSJ
  # MultiJson adapter routing +MultiJson.load+/+dump+ through nosj.
  #
  # multi_json 2.x renamed its namespace MultiJson -> MultiJSON; the
  # adapter inherits from whichever this installation defines.
  #
  # @example
  #   require "nosj/multi_json"
  #   MultiJson.use NOSJ::MultiJsonAdapter
  class MultiJsonAdapter < (defined?(::MultiJSON) ? ::MultiJSON::Adapter : ::MultiJson::Adapter)
    # MultiJson wraps whatever the adapter's ParseError names.
    ParseError = ::JSON::ParserError

    SYMBOLIZE = {symbolize_names: true}.freeze
    private_constant :SYMBOLIZE

    # @param string [String] the JSON document
    # @param options [Hash] multi_json load options; +symbolize_keys+
    #   arrives normalized as +symbolize_names+
    # @return [Object] the parsed value tree
    # @raise [JSON::ParserError] when the document is malformed
    def load(string, options = {})
      ::NOSJ.parse(string, options[:symbolize_names] ? SYMBOLIZE : nil)
    rescue ::NOSJ::ParserError, ::NOSJ::NestingError => e
      raise ParseError, e.message
    end

    # @param object [Object] the value tree to serialize
    # @param options [Hash] multi_json dump options; +pretty+ selects
    #   pretty-printing
    # @return [String] the JSON document
    def dump(object, options = {})
      options[:pretty] ? ::NOSJ.pretty_generate(object) : ::NOSJ.generate(object)
    end
  end
end
