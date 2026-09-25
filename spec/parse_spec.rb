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

  describe "duplicate keys (json 3 semantics)" do
    it "raises by default, positioned at the object repeating the key" do
      expect { NOSJ.parse('{"a":1,"a":2}') }
        .to raise_error(NOSJ::ParserError, 'duplicate key "a" at byte 0')
      # The innermost offending object is reported first, like json 3.
      error = begin
        NOSJ.parse(%({\n  "k": {"z": 0, "z": 1},\n  "k": 2\n}))
      rescue NOSJ::ParserError => e
        e
      end
      expect([error.message, error.line, error.column]).to eq(['duplicate key "z" at byte 9', 2, 8])
    end

    it "keeps the last value under allow_duplicate_key: true, like json 2 and 3" do
      src = '{"a":1,"a":2}'
      expect(NOSJ.parse(src, allow_duplicate_key: true)).to eq({"a" => 2})
      expect(NOSJ.parse(src, allow_duplicate_key: true))
        .to eq(JSON.parse(src, allow_duplicate_key: true))
    end

    it "detects repeats in large objects and symbolized keys too" do
      big = keyed_object(40, repeat: "k7")
      expect { NOSJ.parse(big) }.to raise_error(NOSJ::ParserError, /duplicate key "k7"/)
      expect { NOSJ.parse(big, symbolize_names: true) }.to raise_error(NOSJ::ParserError)
      expect(NOSJ.parse(keyed_object(40)).size).to eq(40)
    end
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

  it "rejects lone surrogates, trailing ones included (json 3 semantics)" do
    expect { NOSJ.parse('["\udc00"]') }
      .to raise_error(NOSJ::ParserError, "lone UTF-16 surrogate at byte 1")
    expect { NOSJ.parse('{"\udc00": 1}') }.to raise_error(NOSJ::ParserError)
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

  it "raises ArgumentError for the unsupported gem options, unless falsy" do
    %i[object_class array_class decimal_class on_load create_additions
      allow_comments allow_control_characters allow_invalid_escape].each do |opt|
      expect { NOSJ.parse("[1]", opt => true) }
        .to raise_error(ArgumentError, "NOSJ does not support the #{opt} option")
      expect(NOSJ.parse("[1]", opt => nil)).to eq([1])
      expect(NOSJ.parse("[1]", opt => false)).to eq([1])
    end
  end

  it "raises json 3's ArgumentError for unknown options" do
    expect { NOSJ.parse("[1]", bogus: 1) }.to raise_error(ArgumentError, "unknown keyword: bogus")
    expect { NOSJ.parse("[1]", :bogus => 1, "symbolize_names" => true, :freeze => true) }
      .to raise_error(ArgumentError, "unknown keywords: bogus, symbolize_names")
    expect { NOSJ.parse("2", quirks_mode: true) }.to raise_error(ArgumentError, "unknown keyword: quirks_mode")
    expect { NOSJ.parse("[1]", indent: "  ") }.to raise_error(ArgumentError, "unknown keyword: indent")
    expect { NOSJ.valid?("[1]", bogus: 1) }.to raise_error(ArgumentError, "unknown keyword: bogus")
    expect { NOSJ.at_pointer("[1]", "/0", bogus: 1) }.to raise_error(ArgumentError, "unknown keyword: bogus")
    expect { NOSJ.lazy("[1]", bogus: 1) }.to raise_error(ArgumentError, "unknown keyword: bogus")
    expect(NOSJ.parse("[1]", {})).to eq([1])
  end
end
