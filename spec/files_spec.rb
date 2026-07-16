# frozen_string_literal: true

require "json"
require "tmpdir"

RSpec.describe "NOSJ file APIs" do
  around do |example|
    Dir.mktmpdir("nosj-files") do |dir|
      @dir = dir
      example.run
    end
  end

  let(:doc) do
    <<~JSON
      {
        "users": [
          {"name": "ada", "tags": ["math", "engines"]},
          {"name": "grace", "score": 99.5}
        ],
        "count": 3
      }
    JSON
  end

  def write_fixture(content, name = "doc.json")
    File.join(@dir, name).tap { |p| File.binwrite(p, content) }
  end

  describe ".load_file" do
    it "matches parse(File.read(path)), options included" do
      path = write_fixture(doc)
      expect(NOSJ.load_file(path)).to eq(NOSJ.parse(doc))
      expect(NOSJ.load_file(path, symbolize_names: true))
        .to eq(NOSJ.parse(doc, symbolize_names: true))
      frozen = NOSJ.load_file(path, freeze: true)
      expect(frozen).to be_frozen
    end

    it "raises Errno for missing files, like File.read" do
      expect { NOSJ.load_file(File.join(@dir, "nope.json")) }
        .to raise_error(Errno::ENOENT, /nope\.json/)
    end

    it "rejects malformed and non-UTF-8 content like parse" do
      expect { NOSJ.load_file(write_fixture('{"a":')) }
        .to raise_error(RuntimeError)
      expect { NOSJ.load_file(write_fixture("\xFF\xFE{}")) }
        .to raise_error(RuntimeError, /UTF-8/)
    end

    it "handles corpus files identically to parse" do
      corpus_files.first(3).each do |path|
        expect(NOSJ.load_file(path)).to eq(NOSJ.parse(File.read(path)))
      end
    end
  end

  describe ".write_file" do
    it "writes exactly what generate produces and returns the byte count" do
      obj = NOSJ.parse(doc)
      path = File.join(@dir, "out.json")
      expected = NOSJ.generate(obj)
      expect(NOSJ.write_file(path, obj)).to eq(expected.bytesize)
      expect(File.read(path)).to eq(expected)
    end

    it "honors generate options" do
      obj = {"a" => [1, true]}
      path = File.join(@dir, "pretty.json")
      NOSJ.write_file(path, obj, indent: "  ", space: " ",
        object_nl: "\n", array_nl: "\n")
      expect(File.read(path)).to eq(NOSJ.pretty_generate(obj))
    end

    it "round-trips through load_file" do
      obj = NOSJ.parse(doc)
      path = File.join(@dir, "roundtrip.json")
      NOSJ.write_file(path, obj)
      expect(NOSJ.load_file(path)).to eq(obj)
    end

    it "raises Errno for unwritable paths and GeneratorError for bad values" do
      expect { NOSJ.write_file(File.join(@dir, "no", "dir.json"), {}) }
        .to raise_error(Errno::ENOENT)
      path = File.join(@dir, "nan.json")
      expect { NOSJ.write_file(path, Float::NAN) }
        .to raise_error(NOSJ::GeneratorError)
    end
  end

  describe ".load_lazy_file" do
    it "wraps the file as a lazy document" do
      path = write_fixture(doc)
      lazy = NOSJ.load_lazy_file(path)
      expect(lazy).to be_a(NOSJ::Lazy)
      expect(lazy["users"][1]["name"]).to eq("grace")
      expect(lazy["users"].size).to eq(2)
      expect(lazy.value).to eq(NOSJ.parse(doc))
    end

    it "applies parse options on materialization" do
      path = write_fixture(doc)
      lazy = NOSJ.load_lazy_file(path, symbolize_names: true)
      expect(lazy["users"][0].value).to eq({name: "ada", tags: %w[math engines]})
    end

    it "keeps the mapping alive through GC, past file deletion" do
      path = write_fixture(doc)
      lazy = NOSJ.load_lazy_file(path)
      File.delete(path)
      GC.start
      expect(lazy["users"][0]["name"]).to eq("ada")
    end

    it "raises Errno for missing files and rejects non-UTF-8 content" do
      expect { NOSJ.load_lazy_file(File.join(@dir, "nope.json")) }
        .to raise_error(Errno::ENOENT)
      expect { NOSJ.load_lazy_file(write_fixture("\xFF\xFE{}")) }
        .to raise_error(RuntimeError, /UTF-8/)
    end
  end

  describe ".at_pointer_file / .dig_file" do
    it "matches the in-memory forms" do
      path = write_fixture(doc)
      expect(NOSJ.at_pointer_file(path, "/users/1/name"))
        .to eq(NOSJ.at_pointer(doc, "/users/1/name"))
      expect(NOSJ.at_pointer_file(path, "/users/0", symbolize_names: true))
        .to eq({name: "ada", tags: %w[math engines]})
      expect(NOSJ.dig_file(path, "users", 0, "tags", 1)).to eq("engines")
      expect(NOSJ.dig_file(path, :count)).to eq(3)
    end

    it "returns nil for misses and negative indices" do
      path = write_fixture(doc)
      expect(NOSJ.at_pointer_file(path, "/nope")).to be_nil
      expect(NOSJ.dig_file(path, "users", 5)).to be_nil
      expect(NOSJ.dig_file(path, "users", -1)).to be_nil
    end

    it "raises ArgumentError for malformed pointers and Errno for missing files" do
      path = write_fixture(doc)
      expect { NOSJ.at_pointer_file(path, "users") }.to raise_error(ArgumentError)
      expect { NOSJ.dig_file(File.join(@dir, "nope.json"), "a") }
        .to raise_error(Errno::ENOENT)
    end
  end
end
