# frozen_string_literal: true

# The drop-in patches JSON globally, so every assertion runs in a
# subprocess. The rest of the suite must keep comparing against the
# pristine json gem (run_script/expect_ok: spec/support/subprocess.rb).
RSpec.describe "require 'nosj/json' drop-in" do
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

      # Accepted input never reaches the gem.
      class << JSON
        %i[nosj_original_parse nosj_original_generate
          nosj_original_pretty_generate nosj_original_dump].each do |m|
          define_method(m) { |*, **| raise m.to_s }
        end
      end
      JSON.parse(src)
      JSON.parse(src, symbolize_names: true, freeze: true)
      JSON.generate(parsed)
      JSON.pretty_generate(parsed)
      JSON.dump(parsed)
      puts "ALL-OK"
    RUBY
  end

  it "fast-paths only options NOSJ reads" do
    # A listed key NOSJ refused would escape as ArgumentError: refusals
    # the drop-in hands back to the gem are parse and generate errors.
    expect_ok(<<~RUBY)
      require "nosj/json"
      NOSJ::JSONDropIn::PARSE_OPTS.each { |key| NOSJ.parse("1", key => nil) }
      NOSJ::JSONDropIn::GENERATE_OPTS.each { |key| NOSJ.generate(1, key => nil) }
      puts "ALL-OK"
    RUBY
  end

  it "follows the installed json gem, 2.x or 3.x: same results, same exceptions" do
    expect_ok(<<~'RUBY')
      require "nosj/json"

      # What one call did: its value, or its exception with everything a
      # rescue clause could look at.
      def outcome
        [:value, yield]
      rescue => e
        details = %i[json_path invalid_object].select { |m| e.respond_to?(m) }.map { |m| e.public_send(m) }
        [:raise, e.class, e.message, details]
      end

      def same(label, method, *args, **opts)
        mine = outcome { JSON.public_send(method, *args, **opts) }
        gems = outcome { JSON.public_send(:"nosj_original_#{method}", *args, **opts) }
        raise "#{label}: #{mine.inspect} vs #{gems.inspect}" unless mine == gems
      end

      # json 3 refuses what json 2 accepted and changed the calling
      # conventions: each case must land where the installed gem does.
      same("duplicate key", :parse, '{"a":1,"a":2}')
      same("nested duplicate key", :parse, '{"x":[{"b":1,"b":2}]}')
      same("allowed duplicate key", :parse, '{"a":1,"a":2}', allow_duplicate_key: true)
      same("lone surrogate", :parse, '["\udc00"]')
      same("comment", :parse, "[1 /* c */]")
      same("syntax error", :parse, '{"a":[1,}')
      same("too deep", :parse, "[" * 101 + "]" * 101)
      same("positional options", :parse, '{"a":1}', {symbolize_names: true})
      same("unknown option", :parse, "[1]", bogus: true)
      same("quirks_mode", :parse, "2", quirks_mode: true)
      same("keys that render alike", :generate, {"a" => 1, :a => 2})
      same("keys that render alike, pretty", :pretty_generate, {"a" => 1, :a => 2})
      same("allowed keys that render alike", :generate, {"a" => 1, :a => 2}, allow_duplicate_key: true)
      same("NaN", :generate, [Float::NAN])
      same("broken UTF-8", :generate, ["\xff".dup.force_encoding(Encoding::UTF_8)])
      same("escape_slash", :generate, ["/"], escape_slash: true)
      same("ascii_only with script_safe", :generate, ["/é "], ascii_only: true, script_safe: true)
      same("ascii_only with script_safe, dumped", :dump, ["/é"], {ascii_only: true, script_safe: true})
      same("unknown generate option", :generate, [1], bogus: true)
      deep = []
      150.times.reduce(deep) { |a, _| (a << []).last }
      same("dump defaults", :dump, deep)
      same("dump options", :dump, {"a" => [1]}, {indent: "  "})
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

      if JSON::VERSION.to_i < 3
        class Point
          attr_reader :x
          def initialize(x) = @x = x
          def self.json_create(h) = new(h["x"])
        end
        pt = JSON.parse('{"json_class":"Point","x":5}', create_additions: true)
        raise "create_additions" unless Point === pt && pt.x == 5
      end

      loaded = []
      JSON.parse('[1,{"a":2}]', on_load: ->(v) { loaded << v; v })
      raise "on_load" unless loaded == [1, "a", 2, {"a" => 2}, [1, {"a" => 2}]]

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
