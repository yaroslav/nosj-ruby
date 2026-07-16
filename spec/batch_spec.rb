# frozen_string_literal: true

require "json"

RSpec.describe "NOSJ.at_pointers / NOSJ.dig_many" do
  let(:doc) do
    <<~JSON
      {
        "users": [
          {"name": "ada", "tags": ["math", "engines"]},
          {"name": "grace", "score": 99.5}
        ],
        "a/b": 1,
        "m~n": {"deep": null},
        "count": 42
      }
    JSON
  end

  describe ".at_pointers" do
    it "returns positionally aligned results, one resolver pass" do
      got = NOSJ.at_pointers(doc, ["/users/1/name", "/count", "/missing", "/users/0/tags"])
      expect(got).to eq(["grace", 42, nil, %w[math engines]])
    end

    it "agrees with sequential at_pointer on every pointer" do
      pointers = ["/users/0", "/users/0/name", "/users/2", "/a~1b", "/m~0n/deep", "/m~0n", "", "/users/1/score"]
      batch = NOSJ.at_pointers(doc, pointers)
      pointers.each_with_index do |ptr, i|
        expect(batch[i]).to eq(NOSJ.at_pointer(doc, ptr)), "pointer #{ptr.inspect}"
      end
    end

    it "handles duplicate pointers and the empty batch" do
      expect(NOSJ.at_pointers(doc, ["/count", "/count"])).to eq([42, 42])
      expect(NOSJ.at_pointers(doc, [])).to eq([])
    end

    it "raises ArgumentError on malformed pointers and non-String entries" do
      expect { NOSJ.at_pointers(doc, ["bad"]) }.to raise_error(ArgumentError)
      expect { NOSJ.at_pointers(doc, [1]) }.to raise_error(ArgumentError)
    end

    it "materializes with parse options" do
      got = NOSJ.at_pointers(doc, ["/users/1"], symbolize_names: true)
      expect(got).to eq([{name: "grace", score: 99.5}])
      frozen = NOSJ.at_pointers(doc, ["/users/0/name"], freeze: true)
      expect(frozen.first).to be_frozen
    end
  end

  describe ".dig_many" do
    it "resolves many paths, aligned, with dig semantics" do
      got = NOSJ.dig_many(doc, [
        ["users", 1, "name"],
        [:count],
        ["users", 0, "tags", 1],
        ["missing", "path"],
        ["users", -1]
      ])
      expect(got).to eq(["grace", 42, "engines", nil, nil])
    end

    it "agrees with sequential dig, including escaped keys" do
      paths = [["a/b"], ["m~n", "deep"], ["users", 5], ["users", 0, "name"]]
      batch = NOSJ.dig_many(doc, paths)
      paths.each_with_index do |path, i|
        expect(batch[i]).to eq(NOSJ.dig(doc, *path)), "path #{path.inspect}"
      end
    end

    it "raises for non-Array paths and bad path elements" do
      expect { NOSJ.dig_many(doc, ["users"]) }.to raise_error(ArgumentError)
      expect { NOSJ.dig_many(doc, [[1.5]]) }.to raise_error(ArgumentError)
    end
  end

  describe ".at_pointer options" do
    it "symbolizes and freezes the materialized subtree" do
      expect(NOSJ.at_pointer(doc, "/users/1", symbolize_names: true)).to eq({name: "grace", score: 99.5})
      value = NOSJ.at_pointer(doc, "/users/0", freeze: true)
      expect(value).to be_frozen
      expect(value["name"]).to be_frozen
    end

    it "still rejects unsupported options" do
      expect { NOSJ.at_pointer(doc, "/count", create_additions: true) }.to raise_error(ArgumentError)
    end
  end
end
