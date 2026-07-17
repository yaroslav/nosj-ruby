# frozen_string_literal: true

require "json"
require "tempfile"

RSpec.describe "NDJSON / JSON Lines" do
  describe "NOSJ.each_line" do
    it "yields one value per line, skipping blank lines" do
      src = %({"a":1}\n\n  \t\n[1,2]\n"s"\n42\ntrue\nnull\n)
      got = []
      expect(NOSJ.each_line(src) { |v| got << v }).to be_nil
      expect(got).to eq([{"a" => 1}, [1, 2], "s", 42, true, nil])
    end

    it "handles CRLF line endings and a missing final newline" do
      expect(NOSJ.each_line(%({"a":1}\r\n{"b":2})).to_a)
        .to eq([{"a" => 1}, {"b" => 2}])
    end

    it "yields nothing for empty and whitespace-only input" do
      expect(NOSJ.each_line("").to_a).to eq([])
      expect(NOSJ.each_line("\n \n\t\n").to_a).to eq([])
    end

    it "returns a lazy Enumerator without a block" do
      src = %({"a":1}\n{"b":2}\n{"c":3}\n)
      enum = NOSJ.each_line(src)
      expect(enum).to be_a(Enumerator)
      expect(enum.to_a).to eq(NOSJ.each_line(src).map { |v| v })
      # Early termination must not walk (or validate) later lines.
      expect(NOSJ.each_line(%({"a":1}\n{"bad": nope}\n)).first(1))
        .to eq([{"a" => 1}])
    end

    it "supports break from the block" do
      result = NOSJ.each_line(%(1\n2\n3\n)) { |v| break v * 10 if v == 2 }
      expect(result).to eq(20)
    end

    it "applies parse options per line" do
      src = %({"a":1}\n{"b":"x"}\n)
      expect(NOSJ.each_line(src, symbolize_names: true).to_a)
        .to eq([{a: 1}, {b: "x"}])
      frozen = NOSJ.each_line(src, freeze: true).to_a
      expect(frozen).to all(be_frozen)
      expect(Ractor.shareable?(frozen.first)).to be(true)
      expect { NOSJ.each_line(%([[1]]\n), max_nesting: 1).to_a }
        .to raise_error(NOSJ::NestingError)
    end

    it "raises rich ParserErrors with the physical line number" do
      src = %({"ok":1}\n{"also": "ok"}\n{"bad": nope}\n{"never":1}\n)
      seen = []
      begin
        NOSJ.each_line(src) { |v| seen << v }
        raise "expected a parse error"
      rescue NOSJ::ParserError => e
        expect(e.line).to eq(3)
        expect(e.byte_offset).to eq(src.index("nope"))
        expect(e.snippet).to include('{"bad": nope}')
      end
      # Lines before the failure were already delivered.
      expect(seen).to eq([{"ok" => 1}, {"also" => "ok"}])
    end

    it "enforces one value per line" do
      expect { NOSJ.each_line(%({"a":1} {"b":2}\n)) {} }
        .to raise_error(NOSJ::ParserError)
    end

    it "survives the block mutating an unfrozen source" do
      src = %({"a":1}\n{"b":2}\n{"c":3}\n).dup
      got = []
      NOSJ.each_line(src) do |v|
        got << v
        src.clear
      end
      expect(got.size).to eq(3)
    end

    it "allows reentrant NOSJ calls from the block" do
      out = NOSJ.each_line(%({"a":1}\n{"b":2}\n)).map do |v|
        NOSJ.generate(NOSJ.parse(NOSJ.generate(v)))
      end
      expect(out).to eq([%({"a":1}), %({"b":2})])
    end

    it "rejects non-UTF-8 input like parse" do
      expect { NOSJ.each_line(%({"a":1}\n).encode(Encoding::UTF_16LE)) {} }
        .to raise_error(NOSJ::ParserError, /UTF-8/)
    end

    it "matches line-split JSON.parse across a hundred real documents" do
      statuses = JSON.parse(File.read(File.join(__dir__, "../benchmark/twitter.json")))["statuses"]
      ndjson = NOSJ.generate_lines(statuses)
      # Interleave blanks and CRLF endings; the values must not change.
      noisy = ndjson.lines.map { |l| "\r\n \n#{l}" }.join
      reference = ndjson.each_line.map { |l| JSON.parse(l) }
      expect(NOSJ.each_line(ndjson).to_a).to eq(reference)
      expect(NOSJ.each_line(noisy).to_a).to eq(reference)
      expect(reference).to eq(statuses)
    end

    it "honors allow_nan and allow_trailing_comma per line" do
      expect { NOSJ.each_line(%([NaN]\n)) {} }.to raise_error(NOSJ::ParserError)
      values = NOSJ.each_line(%([NaN]\n[1,]\n), allow_nan: true, allow_trailing_comma: true).to_a
      expect(values[0][0]).to be_nan
      expect(values[1]).to eq([1])
    end

    it "raises TypeError and ArgumentError like parse" do
      expect { NOSJ.each_line(nil) {} }.to raise_error(TypeError)
      expect { NOSJ.each_line(42) {} }.to raise_error(TypeError)
      expect { NOSJ.each_line(%(1\n), object_class: Hash) {} }
        .to raise_error(ArgumentError, /object_class/)
    end
  end

  describe "NOSJ.generate_lines" do
    it "emits one compact newline-terminated document per element" do
      expect(NOSJ.generate_lines([{"a" => 1}, [2, 3], "s", 42, nil]))
        .to eq(%({"a":1}\n[2,3]\n"s"\n42\nnull\n))
      expect(NOSJ.generate_lines([])).to eq("")
    end

    it "accepts any Enumerable" do
      expect(NOSJ.generate_lines((1..3).lazy.map { |i| {"n" => i} }))
        .to eq(%({"n":1}\n{"n":2}\n{"n":3}\n))
    end

    it "round-trips through each_line, corpus roots included" do
      objs = corpus_files.first(4).map { |f| JSON.parse(File.read(f)) }
      objs += [nil, 42, "s", [1, [2]], {"deep" => {"k" => "v"}}]
      ndjson = NOSJ.generate_lines(objs)
      expect(NOSJ.each_line(ndjson).to_a).to eq(objs)
    end

    it "honors generate options that keep framing intact" do
      expect(NOSJ.generate_lines([{"a" => 1}], space: " "))
        .to eq(%({"a": 1}\n))
      expect(NOSJ.generate_lines([[Float::NAN]], allow_nan: true))
        .to eq("[NaN]\n")
      expect { NOSJ.generate_lines([Object.new], strict: true) }
        .to raise_error(NOSJ::GeneratorError)
    end

    it "rejects formatting options that would break line framing" do
      [{object_nl: "\n"}, {array_nl: "\n"}, {indent: "\t\n"}].each do |opts|
        expect { NOSJ.generate_lines([{"a" => [1]}], opts) }
          .to raise_error(ArgumentError, /framing/), opts.inspect
      end
    end

    it "splices fragments and falls back to to_json, like generate" do
      custom = Class.new { def to_json(*) = %({"custom":true}) }.new
      values = [JSON::Fragment.new(%({"pre":1})), custom]
      expect(NOSJ.generate_lines(values))
        .to eq(%({"pre":1}\n{"custom":true}\n))
    end

    it "raises NestingError on cycles and TypeError on non-enumerables" do
      circular = []
      circular << circular
      expect { NOSJ.generate_lines([circular]) }.to raise_error(NOSJ::NestingError)
      expect { NOSJ.generate_lines(nil) }.to raise_error(TypeError)
      expect { NOSJ.generate_lines(42) }.to raise_error(TypeError)
    end

    it "returns UTF-8" do
      expect(NOSJ.generate_lines([{"é" => "🎉"}]).encoding).to eq(Encoding::UTF_8)
    end
  end

  describe "file forms" do
    it "write_lines + each_line_file round-trip, byte count returned" do
      objs = [{"x" => 1}, {"y" => [2.5, nil]}, "line"]
      Tempfile.create(["lines", ".ndjson"]) do |f|
        n = NOSJ.write_lines(f.path, objs)
        expect(n).to eq(File.size(f.path))
        expect(File.read(f.path)).to eq(NOSJ.generate_lines(objs))
        expect(NOSJ.each_line_file(f.path).to_a).to eq(objs)
        got = []
        expect(NOSJ.each_line_file(f.path) { |v| got << v }).to be_nil
        expect(got).to eq(objs)
      end
    end

    it "raises Errno for missing files" do
      expect { NOSJ.each_line_file("does/not/exist.ndjson") {} }
        .to raise_error(Errno::ENOENT)
    end

    it "reports physical line numbers from files" do
      Tempfile.create(["bad", ".ndjson"]) do |f|
        f.write(%({"a":1}\n{"b": }\n))
        f.flush
        begin
          NOSJ.each_line_file(f.path) {}
          raise "expected a parse error"
        rescue NOSJ::ParserError => e
          expect(e.line).to eq(2)
        end
      end
    end

    it "treats empty and blank-only files as zero-event streams" do
      Tempfile.create(["empty", ".ndjson"]) do |f|
        expect(NOSJ.each_line_file(f.path).to_a).to eq([])
      end
      Tempfile.create(["blank", ".ndjson"]) do |f|
        f.write("\r\n\n  \n")
        f.flush
        expect(NOSJ.each_line_file(f.path).to_a).to eq([])
      end
    end

    it "applies parse options through the file form" do
      Tempfile.create(["opts", ".ndjson"]) do |f|
        f.write(%({"a":1}\n))
        f.flush
        values = NOSJ.each_line_file(f.path, symbolize_names: true, freeze: true).to_a
        expect(values).to eq([{a: 1}])
        expect(Ractor.shareable?(values.first)).to be(true)
      end
    end

    it "write_lines accepts Enumerables, writes empty for empty, rejects bad framing" do
      Tempfile.create(["enum", ".ndjson"]) do |f|
        n = NOSJ.write_lines(f.path, [[1], [2]].each)
        expect(n).to eq(File.size(f.path))
        expect(File.read(f.path)).to eq("[1]\n[2]\n")
        expect(NOSJ.write_lines(f.path, [])).to eq(0)
        expect(File.size(f.path)).to eq(0)
        expect { NOSJ.write_lines(f.path, [1], object_nl: "\n") }
          .to raise_error(ArgumentError, /framing/)
        expect { NOSJ.write_lines(f.path, nil) }.to raise_error(TypeError)
      end
    end
  end
end
