# frozen_string_literal: true

require "json"

RSpec.describe "NOSJ.parse" do
  it "matches JSON.parse on scalars and structures" do
    [
      "null", "true", "false", "0", "-1", "42", "1.5", "-0.25", "1e10",
      '"str"', '"esc \\" \\\\ \\n \\u00e9"', "[]", "{}",
      '{"a":[1,{"b":null}],"c":"d"}', "  [1, 2]  "
    ].each do |src|
      expect(NOSJ.parse(src)).to eq(JSON.parse(src)), "source #{src.inspect}"
    end
  end

  it "matches the json gem across the benchmark corpus" do
    corpus_files.each do |filename|
      json = File.read(filename)
      expect(NOSJ.parse(json)).to eq(JSON.parse(json)), File.basename(filename)
    end
  end

  it "parses big integers exactly" do
    expect(NOSJ.parse((2**80).to_s)).to eq(2**80)
    expect(NOSJ.parse("-#{2**100}")).to eq(-(2**100))
  end

  it "keeps the last value for duplicate keys, like the gem" do
    src = '{"a":1,"a":2}'
    expect(NOSJ.parse(src)).to eq(JSON.parse(src))
    expect(NOSJ.parse(src)).to eq({"a" => 2})
  end

  describe "symbolize_names:" do
    it "symbolizes keys at every level" do
      src = '{"a":{"b":[{"c":1}]},"héllo":2}'
      expect(NOSJ.parse(src, symbolize_names: true))
        .to eq(JSON.parse(src, symbolize_names: true))
    end
  end

  describe "freeze:" do
    it "freezes every value and dedupes strings like the gem" do
      parsed = NOSJ.parse('{"k":["s","s"],"n":{"m":1}}', freeze: true)
      expect(parsed).to be_frozen
      expect(parsed["k"]).to be_frozen
      expect(parsed["n"]).to be_frozen
      a, b = parsed["k"]
      expect(a).to be_frozen
      # fstring identity parity: repeated strings are the same object.
      expect(a).to equal(b)
      expect(parsed.keys.first).to be_frozen
    end
  end

  describe "max_nesting:" do
    let(:deep) { "[" * 101 + "1" + "]" * 101 }

    it "raises NOSJ::NestingError past the gem's default of 100" do
      expect { NOSJ.parse(deep) }.to raise_error(NOSJ::NestingError, /nesting of 101 is too deep/)
      expect { JSON.parse(deep) }.to raise_error(JSON::NestingError)
    end

    it "accepts false for unlimited and Integers as the limit" do
      expect(NOSJ.parse(deep, max_nesting: false)).to eq(JSON.parse(deep, max_nesting: false))
      expect(NOSJ.parse(deep, max_nesting: 200)).to eq(JSON.parse(deep, max_nesting: 200))
      expect { NOSJ.parse("[[1]]", max_nesting: 1) }.to raise_error(NOSJ::NestingError)
    end
  end

  describe "allow_nan:" do
    it "rejects NaN/Infinity by default and accepts them when enabled" do
      expect { NOSJ.parse("[NaN]") }.to raise_error(NOSJ::ParserError)
      parsed = NOSJ.parse("[NaN, Infinity, -Infinity]", allow_nan: true)
      expect(parsed[0]).to be_nan
      expect(parsed[1]).to eq(Float::INFINITY)
      expect(parsed[2]).to eq(-Float::INFINITY)
    end
  end

  describe "allow_trailing_comma:" do
    it "rejects trailing commas by default and accepts them when enabled" do
      expect { NOSJ.parse("[1,2,]") }.to raise_error(NOSJ::ParserError)
      expect(NOSJ.parse("[1,2,]", allow_trailing_comma: true)).to eq([1, 2])
      expect(NOSJ.parse('{"a":1,}', allow_trailing_comma: true)).to eq({"a" => 1})
    end
  end

  it "handles lone surrogates like the gem" do
    # Lone LOW surrogate: both parsers produce the raw WTF-8 bytes.
    low = '"\udc00"'
    expect(NOSJ.parse(low).bytes).to eq(JSON.parse(low).bytes)
    # Lone HIGH surrogate is an error in both.
    expect { NOSJ.parse('"\ud800"') }.to raise_error(NOSJ::ParserError)
    expect { JSON.parse('"\ud800"') }.to raise_error(JSON::ParserError)
    # A proper pair decodes to the astral character.
    expect(NOSJ.parse('"🎉"')).to eq("🎉")
  end

  it "raises NOSJ::ParserError on malformed documents" do
    ['{"a":}', "[1,2", "tru", "", '{"a":1}trailing'].each do |src|
      expect { NOSJ.parse(src) }.to raise_error(NOSJ::ParserError), "source #{src.inspect}"
    end
  end

  it "rejects non-UTF-8 and broken-UTF-8 input" do
    utf16 = "[1]".encode(Encoding::UTF_16LE)
    expect { NOSJ.parse(utf16) }.to raise_error(NOSJ::ParserError, /UTF-8/)
    broken = "\"\xFF\"".dup.force_encoding(Encoding::UTF_8)
    expect { NOSJ.parse(broken) }.to raise_error(NOSJ::ParserError, /UTF-8/)
  end

  it "raises TypeError for non-String input" do
    expect { NOSJ.parse(nil) }.to raise_error(TypeError)
    expect { NOSJ.parse(42) }.to raise_error(TypeError)
  end

  it "raises ArgumentError for the unsupported gem options" do
    %i[object_class array_class decimal_class create_additions].each do |opt|
      expect { NOSJ.parse("[1]", opt => true) }
        .to raise_error(ArgumentError, /does not support the #{opt} option/)
    end
  end
end
