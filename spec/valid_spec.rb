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

  it "agrees with parse on duplicate keys and lone surrogates (json 3 semantics)" do
    # Objects small (pairwise compare), mid-sized (seen-table), and large
    # (sort) take different close-time checks.
    [3, 40, 300].each do |size|
      keys = (1..size).map { %("k#{_1}": #{_1}) }
      unique = "{#{keys.join(",")}}"
      repeated = "{#{keys.join(",")}, \"k#{size / 2}\": 0}"
      expect(NOSJ.valid?(unique)).to be(true), "#{size} unique"
      expect(NOSJ.valid?(repeated)).to be(false), "#{size} repeated"
      expect(NOSJ.valid?(repeated, allow_duplicate_key: true)).to be(true)
      expect { NOSJ.parse(repeated) }.to raise_error(NOSJ::ParserError)
    end
    # A repeat only across sibling objects is fine.
    expect(NOSJ.valid?('[{"a":1},{"a":2}]')).to be(true)
    expect(NOSJ.valid?('{"a":{"a":1}}')).to be(true)
    expect(NOSJ.valid?('["\udc00"]')).to be(false)
  end

  it "refuses and positions repeats and lone surrogates however deep they nest" do
    depth = 5000
    repeated = "[" * depth + '{"a":1,"a":2}' + "]" * depth
    expect(NOSJ.valid?(repeated, max_nesting: false)).to be(false)
    [-> { NOSJ.parse(repeated, max_nesting: false) }, -> { NOSJ.minify(repeated, max_nesting: false) }].each do |call|
      expect(&call).to raise_error(NOSJ::ParserError, %(duplicate key "a" at byte #{depth})) { |e|
        expect(e.byte_offset).to eq(depth)
      }
    end
    lone = "[" * depth + '"\udc00"' + "]" * depth
    expect(NOSJ.valid?(lone, max_nesting: false)).to be(false)
    expect { NOSJ.parse(lone, max_nesting: false) }
      .to raise_error(NOSJ::ParserError, "lone UTF-16 surrogate at byte #{depth}")
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
