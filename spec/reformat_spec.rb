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

  it "preserves duplicate keys and big-integer digits verbatim" do
    expect(NOSJ.minify(%({"a": 1, "a": 2}))).to eq(%({"a":1,"a":2}))
    digits = "123456789012345678901234567890"
    expect(NOSJ.minify(%([#{digits}]))).to eq("[#{digits}]")
  end

  it "re-escapes lone-surrogate values so output always reparses" do
    expect(NOSJ.minify(%("\\udc00"))).to eq(%("\\udc00"))
    mixed = %("a\\udc00é")
    expect(NOSJ.parse(NOSJ.minify(mixed)).bytes).to eq(NOSJ.parse(mixed).bytes)
    expect(NOSJ.reformat(%(["\\udc00"]), ascii_only: true)).to eq(%(["\\udc00"]))
    expect { NOSJ.minify(%({"\\udc00": 1})) }
      .to raise_error(NOSJ::GeneratorError, /malformed utf-8/)
  end

  it "honors acceptance options and normalizes what they accept" do
    expect(NOSJ.minify("[1, 2,]", allow_trailing_comma: true)).to eq("[1,2]")
    expect(NOSJ.minify("[NaN, -Infinity]", allow_nan: true)).to eq("[NaN,-Infinity]")
    expect { NOSJ.minify("[1,]") }.to raise_error(NOSJ::ParserError)
    expect { NOSJ.minify("[[[1]]]", max_nesting: 2) }.to raise_error(NOSJ::NestingError)
    deep = "[" * 150 + "1" + "]" * 150
    expect(NOSJ.minify(deep, max_nesting: false)).to eq(deep)
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
