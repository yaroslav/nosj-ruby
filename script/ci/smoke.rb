# frozen_string_literal: true

# Installed-gem smoke test: exercises parse, generate, validation, and
# partial parsing through the packaged extension.
require "nosj"

abort "parse" unless NOSJ.parse('{"a":[1,true]}') == {"a" => [1, true]}
abort "symbolize" unless NOSJ.parse('{"a":1}', symbolize_names: true) == {a: 1}
abort "generate" unless NOSJ.generate({"b" => 1.5}) == '{"b":1.5}'
abort "pretty" unless NOSJ.pretty_generate({"b" => 1}) == "{\n  \"b\": 1\n}"
abort "valid" unless NOSJ.valid?("[1]") && !NOSJ.valid?("[")
abort "pointer" unless NOSJ.at_pointer('{"u":[{"n":"g"}]}', "/u/0/n") == "g"
abort "dig" unless NOSJ.dig('{"u":[{"n":"g"}]}', "u", 0, "n") == "g"
abort "batch" unless NOSJ.at_pointers('{"a":1,"b":2}', ["/b", "/x"]) == [2, nil]

# The JSON drop-in must ship in every gem and take over JSON.parse.
require "nosj/json"
abort "drop-in alias" unless JSON.respond_to?(:nosj_original_parse)
abort "drop-in parse" unless JSON.parse('{"c":3}') == {"c" => 3}

puts "smoke test passed (#{RUBY_DESCRIPTION})"
