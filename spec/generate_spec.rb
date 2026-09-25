# frozen_string_literal: true

require "json"

RSpec.describe "NOSJ.generate" do
  def expect_gem_parity(obj, opts = nil)
    gem_out = opts ? JSON.generate(obj, **opts) : JSON.generate(obj)
    spin_out = NOSJ.generate(obj, opts)
    expect(spin_out).to eq(gem_out)
  end

  it "matches JSON.generate on scalars" do
    [nil, true, false, 0, 42, -1, 2**80, -(2**100), "plain", :sym].each do |v|
      expect_gem_parity(v)
      expect_gem_parity([v])
    end
  end

  it "matches JSON.generate float formatting byte-for-byte" do
    [0.0, -0.0, 5.0, 1.5, 0.0001, 1e-5, 1e14, 1e15, 1e16, 1.5e-7, 1e100,
      1.0 / 3.0, 2.34387207031, -61.14917000000003, 0.130293816489,
      1.7976931348623157e308, 5e-324].each do |f|
      expect_gem_parity([f])
    end
  end

  it "matches JSON.generate on structures and escapes" do
    expect_gem_parity({"a" => [1, {"x" => 2}, "s"], "e" => {}, "n" => nil})
    expect_gem_parity(["with \"quotes\" \\ \n\t\r", "héllo 🎉"])
    expect_gem_parity({1 => 2, 2.5 => 3, nil => 4, :sym => 5})
    expect_gem_parity(["x" * 5000 + '"' + "y" * 5000])
  end

  it "matches JSON.pretty_generate" do
    obj = {"a" => [1, {"x" => 2}], "e" => {}, "ea" => []}
    expect(NOSJ.pretty_generate(obj)).to eq(JSON.pretty_generate(obj))
  end

  it "supports the gem's formatting options" do
    obj = {"k" => [1, {}]}
    opts = {indent: "\t", space: " ", space_before: " ", object_nl: "\n", array_nl: "\n"}
    expect_gem_parity(obj, opts)
    expect_gem_parity(obj, {indent: "..", object_nl: "|"})
    expect_gem_parity(["a/b"], {script_safe: true})
    expect_gem_parity(["héllo κόσμε 🎉"], {ascii_only: true})
  end

  it "handles NaN/Infinity per allow_nan" do
    expect { NOSJ.generate([Float::NAN]) }
      .to raise_error(NOSJ::GeneratorError, "NaN not allowed in JSON")
    expect(NOSJ.generate([Float::NAN, Float::INFINITY, -Float::INFINITY], allow_nan: true))
      .to eq("[NaN,Infinity,-Infinity]")
  end

  it "enforces max_nesting and detects cycles" do
    deep = []
    cur = deep
    100.times {
      n = []
      cur << n
      cur = n
    }
    expect { NOSJ.generate(deep) }.to raise_error(NOSJ::NestingError, /nesting of 100 is too deep/)
    expect(NOSJ.generate(deep, max_nesting: false)).to eq(JSON.generate(deep, max_nesting: false))
    circular = []
    circular << circular
    expect { NOSJ.generate(circular) }.to raise_error(NOSJ::NestingError)
  end

  it "supports strict mode" do
    expect { NOSJ.generate([Object.new], strict: true) }
      .to raise_error(NOSJ::GeneratorError, "Object not allowed in JSON")
    expect(NOSJ.generate([:sym, 1], strict: true)).to eq('["sym",1]')
  end

  it "matches gem encoding behavior" do
    expect_gem_parity(["é".encode("ISO-8859-1")])
    expect { NOSJ.generate(["\xff".dup.force_encoding("BINARY")]) }
      .to raise_error(NOSJ::GeneratorError, /ASCII-8BIT to UTF-8/)
    expect { NOSJ.generate(["\xff".dup.force_encoding("UTF-8")]) }
      .to raise_error(NOSJ::GeneratorError, "source sequence is illegal/malformed utf-8")
  end

  it "falls back to to_json / to_s for foreign objects" do
    klass = Class.new { def to_json(*) = '{"custom":true}' }
    expect(NOSJ.generate([klass.new, 1])).to eq('[{"custom":true},1]')
  end

  it "round-trips benchmark files byte-identically to the gem" do
    corpus_files.each do |f|
      obj = JSON.parse(File.read(f))
      expect(NOSJ.generate(obj)).to eq(JSON.generate(obj)), "compact mismatch: #{File.basename(f)}"
      expect(NOSJ.parse(NOSJ.generate(obj))).to eq(obj), "round-trip mismatch: #{File.basename(f)}"
    end
  end

  it "splices JSON::Fragment like the gem, in default and strict modes" do
    value = {"cached" => JSON::Fragment.new('{"pre":"rendered"}')}
    expect_gem_parity(value)
    expect(NOSJ.generate(value, strict: true))
      .to eq(JSON.generate(value, strict: true))
    expect(NOSJ.generate(value)).to eq('{"cached":{"pre":"rendered"}}')
  end

  it "raises json 3's ArgumentError for unknown options, and for unsupported ones unless falsy" do
    expect { NOSJ.generate([1], bogus: 1) }.to raise_error(ArgumentError, "unknown keyword: bogus")
    expect { NOSJ.pretty_generate([1], bogus: 1) }.to raise_error(ArgumentError, "unknown keyword: bogus")
    expect { NOSJ.generate(["/"], escape_slash: true) }
      .to raise_error(ArgumentError, "unknown keyword: escape_slash")
    expect { NOSJ.generate([1], symbolize_names: true) }
      .to raise_error(ArgumentError, "unknown keyword: symbolize_names")
    expect { NOSJ.generate_lines([1], bogus: 1) }.to raise_error(ArgumentError, "unknown keyword: bogus")
    expect { NOSJ.splice("[1]", {"/0" => 2}, bogus: 1) }.to raise_error(ArgumentError, "unknown keyword: bogus")
    %i[sort_keys as_json].each do |opt|
      expect { NOSJ.generate([1], opt => true) }
        .to raise_error(ArgumentError, "NOSJ does not support the #{opt} option")
      expect(NOSJ.generate([1], opt => false)).to eq("[1]")
    end
  end

  describe "keys that render alike (json 3 semantics)" do
    # json 3's rule: only a String or Symbol key in a hash whose first key
    # had another kind triggers the check (keys of one kind cannot
    # collide), which then compares every key's to_s.
    it "raises the gem's GeneratorError for mixed-kind collisions" do
      inner = {"c" => 1, :c => 2}
      [
        [{"a" => 1, :a => 2}, "a"],
        [{:b => 1, :a => 2, "a" => 3}, "a"],
        [{1 => 1, "1" => 2}, "1"],
        [{nil => 1, "" => 2}, ""],
        [{"x" => inner}, "c", inner]
      ].each do |hash, key, offender = hash|
        # json 3 builds the message from #inspect, whose Hash format
        # changed in Ruby 3.4 ({"a" => 1, a: 2} vs {"a"=>1, :a=>2}).
        message = "detected duplicate key #{key.inspect} in #{offender.inspect}"
        expect { NOSJ.generate(hash) }.to raise_error(NOSJ::GeneratorError, message)
        expect { NOSJ.pretty_generate(hash) }.to raise_error(NOSJ::GeneratorError, message)
      end
    end

    it "emits them under allow_duplicate_key: true, and never checks one-kind hashes" do
      expect(NOSJ.generate({"a" => 1, :a => 2}, allow_duplicate_key: true)).to eq('{"a":1,"a":2}')
      expect(NOSJ.generate({"a" => 1, :b => 2})).to eq('{"a":1,"b":2}')
      same = Struct.new(:n) { def to_s = "same" }
      expect(NOSJ.generate({same.new(1) => 1, same.new(2) => 2})).to eq('{"same":1,"same":2}')
      identity = {}.compare_by_identity
      identity[+"a"] = 1
      identity[+"a"] = 2
      expect(NOSJ.generate(identity)).to eq('{"a":1,"a":2}')
    end
  end
end
