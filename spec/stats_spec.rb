# frozen_string_literal: true

require "json"
require "tempfile"

RSpec.describe "NOSJ.stats" do
  # Reference implementation over a parsed tree; the native pass must
  # agree with it on every corpus file.
  def reference_stats(value, acc = nil, depth = 0)
    acc ||= {
      objects: 0, arrays: 0, strings: 0, integers: 0, floats: 0,
      booleans: 0, nulls: 0, keys: 0, histogram: Hash.new(0),
      max_depth: 0, max_object_entries: 0, max_array_length: 0,
      string_bytes: 0, max_string_bytes: 0
    }
    case value
    when Hash
      acc[:objects] += 1
      acc[:max_depth] = [acc[:max_depth], depth + 1].max
      acc[:max_object_entries] = [acc[:max_object_entries], value.size].max
      value.each do |k, v|
        acc[:keys] += 1
        acc[:histogram][k] += 1
        reference_stats(v, acc, depth + 1)
      end
    when Array
      acc[:arrays] += 1
      acc[:max_depth] = [acc[:max_depth], depth + 1].max
      acc[:max_array_length] = [acc[:max_array_length], value.size].max
      value.each { |v| reference_stats(v, acc, depth + 1) }
    when String
      acc[:strings] += 1
      acc[:string_bytes] += value.bytesize
      acc[:max_string_bytes] = [acc[:max_string_bytes], value.bytesize].max
    when Integer then acc[:integers] += 1
    when Float then acc[:floats] += 1
    when true, false then acc[:booleans] += 1
    when nil then acc[:nulls] += 1
    end
    acc
  end

  it "returns the full description of a document" do
    src = %({"users":[{"name":"ada","age":36,"tags":["a","b"]},) +
      %({"name":"grace","age":null,"score":1.5,"ok":true}],"total":2})
    expect(NOSJ.stats(src)).to eq({
      byte_size: src.bytesize,
      root: :object,
      max_depth: 4,
      values: {total: 14, objects: 3, arrays: 2, strings: 4,
               integers: 2, floats: 1, booleans: 1, nulls: 1},
      keys: {total: 9, unique: 7},
      key_histogram: {"age" => 2, "name" => 2, "ok" => 1, "score" => 1,
                      "tags" => 1, "total" => 1, "users" => 1},
      containers: {max_object_entries: 4, max_array_length: 2},
      strings: {bytes: 10, max_bytes: 5}
    })
  end

  it "agrees with a reference implementation across the corpus" do
    corpus_files.each do |path|
      src = File.read(path)
      ref = reference_stats(JSON.parse(src))
      s = NOSJ.stats(src)
      name = File.basename(path)

      expect(s[:byte_size]).to eq(src.bytesize), name
      expect(s[:max_depth]).to eq(ref[:max_depth]), name
      expect(s[:values]).to eq({
        total: %i[objects arrays strings integers floats booleans nulls]
          .sum { |k| ref[k] },
        objects: ref[:objects], arrays: ref[:arrays],
        strings: ref[:strings], integers: ref[:integers],
        floats: ref[:floats], booleans: ref[:booleans], nulls: ref[:nulls]
      }), name
      expect(s[:keys]).to eq({total: ref[:keys], unique: ref[:histogram].size}), name
      expect(s[:key_histogram]).to eq(ref[:histogram]), name
      expect(s[:containers]).to eq({
        max_object_entries: ref[:max_object_entries],
        max_array_length: ref[:max_array_length]
      }), name
      expect(s[:strings]).to eq({
        bytes: ref[:string_bytes], max_bytes: ref[:max_string_bytes]
      }), name
    end
  end

  it "sorts the key histogram by count, ties by key" do
    hist = NOSJ.stats(%({"b":1,"a":1,"c":{"a":1,"c":2,"c2":{"c":3}}}))[:key_histogram]
    expect(hist.keys).to eq(%w[c a b c2])
    expect(hist.values).to eq([3, 2, 1, 1])
  end

  it "reports scalar roots with zero depth" do
    {
      "42" => :integer, "1.5" => :float, '"s"' => :string,
      "true" => :boolean, "false" => :boolean, "null" => :null
    }.each do |src, kind|
      s = NOSJ.stats(src)
      expect(s[:root]).to eq(kind), src
      expect(s[:max_depth]).to eq(0), src
      expect(s[:values][:total]).to eq(1), src
    end
    expect(NOSJ.stats("[1]")[:root]).to eq(:array)
  end

  it "counts big integers as integers" do
    s = NOSJ.stats(%({"n": #{2**100}}))
    expect(s[:values][:integers]).to eq(1)
  end

  it "does not limit nesting unless asked" do
    deep = "[" * 300 + "]" * 300
    expect(NOSJ.stats(deep)[:max_depth]).to eq(300)
    expect { NOSJ.stats(deep, max_nesting: 100) }
      .to raise_error(NOSJ::NestingError, /nesting of 101 is too deep/)
    expect(NOSJ.stats(deep, max_nesting: false)[:max_depth]).to eq(300)
  end

  it "honors the acceptance options" do
    expect { NOSJ.stats("[NaN]") }.to raise_error(NOSJ::ParserError)
    expect(NOSJ.stats("[NaN]", allow_nan: true)[:values][:floats]).to eq(1)
    expect(NOSJ.stats("[1,]", allow_trailing_comma: true)[:values][:integers]).to eq(1)
    expect { NOSJ.stats("[1]", object_class: Hash) }.to raise_error(ArgumentError)
  end

  it "raises rich ParserErrors like parse" do
    begin
      NOSJ.stats(%({"a": nope}))
      raise "expected a parse error"
    rescue NOSJ::ParserError => e
      expect(e.byte_offset).to eq(6)
      expect(e.snippet).to include("nope")
    end
    expect { NOSJ.stats("{}".encode(Encoding::UTF_16LE)) }
      .to raise_error(NOSJ::ParserError, /UTF-8/)
  end

  describe "NOSJ.stats_file" do
    it "matches stats of the file content and reports the file size" do
      corpus = corpus_files.first
      expect(NOSJ.stats_file(corpus)).to eq(NOSJ.stats(File.read(corpus)))
      expect(NOSJ.stats_file(corpus)[:byte_size]).to eq(File.size(corpus))
    end

    it "raises Errno for missing files and ParserError for bad content" do
      expect { NOSJ.stats_file("does/not/exist.json") }
        .to raise_error(Errno::ENOENT)
      Tempfile.create(["stats", ".json"]) do |f|
        f.write("{nope")
        f.flush
        expect { NOSJ.stats_file(f.path) }.to raise_error(NOSJ::ParserError)
      end
    end
  end
end
