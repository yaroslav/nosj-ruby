# frozen_string_literal: true

# The drop-in patches JSON globally, so every assertion runs in a
# subprocess. The rest of the suite must keep comparing against the
# pristine json gem.
RSpec.describe "require 'nosj/json' drop-in" do
  def run_script(script)
    out = IO.popen(
      [RbConfig.ruby, "-I", File.expand_path("../lib", __dir__), "-e", script],
      err: [:child, :out], &:read
    )
    [$?.success?, out]
  end

  def expect_ok(script)
    ok, out = run_script(script)
    expect(ok).to be(true), out
    expect(out).to include("ALL-OK")
  end

  it "reroutes the supported fast paths and matches the original gem byte-for-byte" do
    corpus = File.expand_path("../benchmark/twitter.json", __dir__)
    expect_ok(<<~RUBY)
      require "nosj/json"
      src = File.read(#{corpus.inspect})
      parsed = JSON.parse(src)
      raise "parse mismatch" unless parsed == JSON.nosj_original_parse(src)
      raise "generate mismatch" unless JSON.generate(parsed) == JSON.nosj_original_generate(parsed)
      raise "pretty mismatch" unless JSON.pretty_generate(parsed) == JSON.nosj_original_pretty_generate(parsed)
      raise "dump mismatch" unless JSON.dump(parsed) == JSON.nosj_original_dump(parsed)
      raise "symbolize" unless JSON.parse('{"a":1}', symbolize_names: true) == {a: 1}
      puts "ALL-OK"
    RUBY
  end

  it "keeps JSON exception classes rescuable" do
    expect_ok(<<~RUBY)
      require "nosj/json"
      begin
        JSON.parse("{")
        raise "no error"
      rescue JSON::ParserError
      end
      begin
        JSON.generate(Object.new, strict: true)
        raise "no error"
      rescue JSON::GeneratorError
      end
      begin
        JSON.generate([[[1]]], max_nesting: 1)
        raise "no error"
      rescue JSON::NestingError
      end
      puts "ALL-OK"
    RUBY
  end

  it "falls back to the original implementation for unsupported options" do
    expect_ok(<<~RUBY)
      require "nosj/json"

      class Point
        attr_reader :x
        def initialize(x) = @x = x
        def self.json_create(h) = new(h["x"])
      end
      pt = JSON.parse('{"json_class":"Point","x":5}', create_additions: true)
      raise "create_additions" unless Point === pt && pt.x == 5

      class MyHash < Hash; end
      raise "object_class" unless JSON.parse('{"a":1}', object_class: MyHash).instance_of?(MyHash)

      state = JSON::State.new(indent: "  ", object_nl: "\\n")
      raise "state" unless JSON.generate({"a" => 1}, state).include?("\\n")

      require "stringio"
      io = StringIO.new
      JSON.dump({"a" => 1}, io)
      raise "dump io" unless io.string == '{"a":1}'
      puts "ALL-OK"
    RUBY
  end

  it "keeps the derived entry points working (load, parse!, load_file, dump defaults)" do
    expect_ok(<<~RUBY)
      require "nosj/json"
      require "tempfile"

      raise "load" unless JSON.load('{"a":1}') == {"a" => 1}
      raise "load nil" unless JSON.load(nil).nil?

      deep = "[" * 150 + "]" * 150
      raise "parse!" unless JSON.parse!(deep).is_a?(Array)

      raise "dump nan" unless JSON.dump(Float::NAN) == "NaN"

      Tempfile.create(["dropin", ".json"]) do |f|
        f.write('{"k":[1,2]}')
        f.flush
        raise "load_file" unless JSON.load_file(f.path) == {"k" => [1, 2]}
      end
      puts "ALL-OK"
    RUBY
  end

  it "accepts the encodings the gem accepts (Rack bodies are BINARY)" do
    expect_ok(<<~RUBY)
      require "nosj/json"
      body = '{"user":"ada","n":1.5}'.b
      raise "binary" unless JSON.parse(body) == {"user" => "ada", "n" => 1.5}
      utf16 = '{"a":1}'.encode(Encoding::UTF_16LE)
      raise "utf16 fallback" unless JSON.parse(utf16) == JSON.nosj_original_parse(utf16)
      begin
        JSON.parse("\\xFF\\xFE{}".b)
        raise "no error"
      rescue JSON::ParserError
      end
      puts "ALL-OK"
    RUBY
  end

  it "provides a MultiJson adapter" do
    expect_ok(<<~RUBY)
      require "nosj/multi_json"
      MultiJson.use NOSJ::MultiJsonAdapter
      raise "load" unless MultiJson.load('{"a":1}') == {"a" => 1}
      raise "symbolize" unless MultiJson.load('{"a":1}', symbolize_keys: true) == {a: 1}
      raise "dump" unless MultiJson.dump({"a" => 1}) == '{"a":1}'
      begin
        MultiJson.load("{")
        raise "no error"
      rescue MultiJson::ParseError
      end
      puts "ALL-OK"
    RUBY
  end
end
