# frozen_string_literal: true

require "json"
require "tempfile"

RSpec.describe "NOSJ.minify / NOSJ.reformat" do
  it "matches generate(parse(x)) across the whole corpus" do
    corpus_files.each do |path|
      src = File.read(path)
      name = File.basename(path)
      expect(NOSJ.minify(src)).to eq(NOSJ.generate(NOSJ.parse(src))), name
      expect(NOSJ.reformat(src, pretty: true))
        .to eq(NOSJ.pretty_generate(NOSJ.parse(src))), name
    end
  end

  it "allocates no per-document Ruby objects" do
    src = File.read(File.join(__dir__, "../benchmark/twitter.json")).freeze
    NOSJ.minify(src)
    before = GC.stat(:total_allocated_objects)
    NOSJ.minify(src)
    expect(GC.stat(:total_allocated_objects) - before).to be < 10
  end

  it "normalizes whitespace, escapes, and number spellings" do
    src = %({ "a":\t[ 1.50, 1e2, "\\u0041" ] })
    expect(NOSJ.minify(src)).to eq(%({"a":[1.5,100.0,"A"]}))
  end

  it "refuses duplicate keys like parse, and preserves them under allow_duplicate_key" do
    expect { NOSJ.minify(%({"a": 1, "a": 2})) }
      .to raise_error(NOSJ::ParserError, 'duplicate key "a" at byte 0')
    expect(NOSJ.minify(%({"a": 1, "a": 2}), allow_duplicate_key: true)).to eq(%({"a":1,"a":2}))
    keys = (1..40).map { %("k#{_1}": #{_1}) }
    expect { NOSJ.minify("{#{keys.join(",")}, \"k40\": 0}") }.to raise_error(NOSJ::ParserError)
    expect(NOSJ.minify("{#{keys.join(",")}}")).to eq(NOSJ.generate(NOSJ.parse("{#{keys.join(",")}}")))
  end

  it "preserves big-integer digits verbatim" do
    digits = "123456789012345678901234567890"
    expect(NOSJ.minify(%([#{digits}]))).to eq("[#{digits}]")
  end

  it "refuses lone surrogates like parse" do
    expect { NOSJ.minify(%(["\\udc00"])) }
      .to raise_error(NOSJ::ParserError, "lone UTF-16 surrogate at byte 1")
    expect { NOSJ.minify(%({"\\udc00": 1})) }.to raise_error(NOSJ::ParserError)
  end

  it "honors acceptance options and normalizes what they accept" do
    expect(NOSJ.minify("[1, 2,]", allow_trailing_comma: true)).to eq("[1,2]")
    expect(NOSJ.minify("[NaN, -Infinity]", allow_nan: true)).to eq("[NaN,-Infinity]")
    expect { NOSJ.minify("[1,]") }.to raise_error(NOSJ::ParserError)
    expect { NOSJ.minify("[[[1]]]", max_nesting: 2) }.to raise_error(NOSJ::NestingError)
    deep = "[" * 150 + "1" + "]" * 150
    expect(NOSJ.minify(deep, max_nesting: false)).to eq(deep)
  end

  it "refuses overflow-to-Infinity floats like generate (fuzz find)" do
    # 1e999 parses to Infinity even in strict mode (gem parity), so a
    # bare literal from the pipe would not reparse.
    expect { NOSJ.minify("[1e999]") }
      .to raise_error(NOSJ::GeneratorError, "Infinity not allowed in JSON")
    expect { NOSJ.minify("[-1e999]") }
      .to raise_error(NOSJ::GeneratorError, "-Infinity not allowed in JSON")
    expect(NOSJ.minify("[1e999]", allow_nan: true)).to eq("[Infinity]")
    expect(NOSJ.generate(NOSJ.parse("[1e999]"), allow_nan: true)).to eq("[Infinity]")
    # Under allow_duplicate_key the pipe streams entries parse would
    # discard, so an overflowing literal shadowed by a duplicate still
    # refuses even though generate(parse(x)) succeeds (fuzz find).
    shadowed = %({"b": [1e999], "b": 1})
    expect(NOSJ.generate(NOSJ.parse(shadowed, allow_duplicate_key: true))).to eq(%({"b":1}))
    expect { NOSJ.minify(shadowed, allow_duplicate_key: true) }
      .to raise_error(NOSJ::GeneratorError, "Infinity not allowed in JSON")
  end

  it "composes pretty with explicit formatting overrides" do
    src = %({"a":[1]})
    expect(NOSJ.reformat(src, pretty: true, indent: "\t"))
      .to eq(NOSJ.generate(NOSJ.parse(src),
        indent: "\t", space: " ", object_nl: "\n", array_nl: "\n"))
    expect(NOSJ.reformat(src, indent: "..", object_nl: "|"))
      .to eq(NOSJ.generate(NOSJ.parse(src), indent: "..", object_nl: "|"))
    expect(NOSJ.reformat(src, pretty: false)).to eq(NOSJ.minify(src))
  end

  it "applies escape transcoding options" do
    src = %({"s":"héllo 🎉","p":"a/b"})
    expect(NOSJ.reformat(src, ascii_only: true))
      .to eq(NOSJ.generate(NOSJ.parse(src), ascii_only: true))
    expect(NOSJ.reformat(src, script_safe: true))
      .to eq(NOSJ.generate(NOSJ.parse(src), script_safe: true))
  end

  it "raises rich ParserErrors and rejects non-UTF-8 like parse" do
    begin
      NOSJ.minify(%({\n "a": nope}))
      raise "expected a parse error"
    rescue NOSJ::ParserError => e
      expect(e.line).to eq(2)
      expect(e.snippet).to include("nope")
    end
    expect { NOSJ.minify("[1]".encode(Encoding::UTF_16LE)) }
      .to raise_error(NOSJ::ParserError, /UTF-8/)
    expect { NOSJ.minify(nil) }.to raise_error(TypeError)
  end

  describe "NOSJ.reformat_file" do
    it "reformats straight off a memory map" do
      Tempfile.create(["fmt", ".json"]) do |f|
        f.write(%({ "a": [1, 2] }))
        f.flush
        expect(NOSJ.reformat_file(f.path)).to eq(%({"a":[1,2]}))
        expect(NOSJ.reformat_file(f.path, pretty: true))
          .to eq(NOSJ.pretty_generate({"a" => [1, 2]}))
      end
    end

    it "raises Errno for missing files and ParserError for empty ones" do
      expect { NOSJ.reformat_file("does/not/exist.json") }
        .to raise_error(Errno::ENOENT)
      Tempfile.create(["empty", ".json"]) do |f|
        expect { NOSJ.reformat_file(f.path) }.to raise_error(NOSJ::ParserError)
      end
    end
  end
end
