# frozen_string_literal: true

require "json"

RSpec.describe "NOSJ.valid?" do
  it "accepts every document NOSJ.parse accepts" do
    [
      '{"a":1}', "[1,2,3]", '"str"', "12.5", "true", "null", "{}", "[]",
      '{"nested":{"deep":[1,{"x":null}]}}',
      '"éscapé"',
      "  [1]  "
    ].each do |src|
      expect(NOSJ.valid?(src)).to be(true), "expected valid: #{src.inspect}"
      expect { NOSJ.parse(src) }.not_to raise_error
    end
  end

  it "rejects every document NOSJ.parse rejects" do
    [
      '{"a":}', "[1,2,", "tru", '"unterminated', "1.2.3", "{", "",
      '{"a":1}garbage', "[1,2,]", "NaN", "'single'"
    ].each do |src|
      expect(NOSJ.valid?(src)).to be(false), "expected invalid: #{src.inspect}"
      expect { NOSJ.parse(src) }.to raise_error(StandardError)
    end
  end

  it "agrees with parse across the benchmark corpus" do
    corpus_files.each do |f|
      expect(NOSJ.valid?(File.read(f))).to be(true), File.basename(f)
    end
  end

  it "honors allow_trailing_comma and allow_nan like parse" do
    expect(NOSJ.valid?("[1,2,]")).to be(false)
    expect(NOSJ.valid?("[1,2,]", allow_trailing_comma: true)).to be(true)
    expect(NOSJ.valid?("[NaN]")).to be(false)
    expect(NOSJ.valid?("[NaN]", allow_nan: true)).to be(true)
  end

  it "honors max_nesting like parse" do
    deep = "[" * 101 + "]" * 101
    expect(NOSJ.valid?(deep)).to be(false)
    expect(NOSJ.valid?(deep, max_nesting: false)).to be(true)
    expect(NOSJ.valid?(deep, max_nesting: 200)).to be(true)
    expect(NOSJ.valid?("[[1]]", max_nesting: 1)).to be(false)
  end

  it "returns false for non-UTF-8 input instead of raising" do
    expect(NOSJ.valid?("[1]".encode(Encoding::UTF_16LE).force_encoding(Encoding::UTF_16LE))).to be(false)
    expect(NOSJ.valid?("\"\xFF\"".dup.force_encoding(Encoding::UTF_8))).to be(false)
  end

  it "raises for non-String input and bad options, like parse" do
    expect { NOSJ.valid?(nil) }.to raise_error(TypeError)
    expect { NOSJ.valid?("[1]", create_additions: true) }.to raise_error(ArgumentError)
  end
end
