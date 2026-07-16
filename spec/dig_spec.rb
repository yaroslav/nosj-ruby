# frozen_string_literal: true

require "json"

RSpec.describe "NOSJ.dig / NOSJ.at_pointer" do
  let(:doc) do
    <<~JSON
      {
        "users": [
          {"name": "ada", "tags": ["math", "engines"]},
          {"name": "grace", "score": 99.5, "active": true}
        ],
        "a/b": 1,
        "m~n": {"deep": null},
        "count": 12345678901234567890
      }
    JSON
  end

  describe ".dig" do
    it "matches Hash#dig on parsed output for present paths" do
      [
        ["users"],
        ["users", 0],
        ["users", 0, "name"],
        ["users", 1, "score"],
        ["users", 1, "active"],
        ["users", 0, "tags", 1],
        ["count"]
      ].each do |path|
        expect(NOSJ.dig(doc, *path)).to eq(JSON.parse(doc).dig(*path)),
          "path #{path.inspect}"
      end
    end

    it "accepts Symbol keys" do
      expect(NOSJ.dig(doc, :users, 1, :name)).to eq("grace")
    end

    it "returns nil for missing paths" do
      expect(NOSJ.dig(doc, "missing")).to be_nil
      expect(NOSJ.dig(doc, "users", 9)).to be_nil
      expect(NOSJ.dig(doc, "users", 0, "name", "deeper")).to be_nil
      expect(NOSJ.dig(doc, "users", "not-an-index")).to be_nil
    end

    it "returns nil for negative indices (documented divergence)" do
      expect(NOSJ.dig(doc, "users", -1)).to be_nil
    end

    it "escapes ~ and / in keys" do
      expect(NOSJ.dig(doc, "a/b")).to eq(1)
      expect(NOSJ.dig(doc, "m~n", "deep")).to be_nil # value is null
      expect(NOSJ.dig(doc, "m~n")).to eq({"deep" => nil})
    end

    it "returns the whole document for an empty path" do
      expect(NOSJ.dig(doc)).to eq(JSON.parse(doc))
    end

    it "raises on invalid JSON" do
      expect { NOSJ.dig('{"a":', "a") }.to raise_error(RuntimeError)
    end

    it "raises ArgumentError for unsupported path element types" do
      expect { NOSJ.dig(doc, 1.5) }.to raise_error(ArgumentError)
    end
  end

  describe ".at_pointer" do
    it "resolves RFC 6901 pointers" do
      expect(NOSJ.at_pointer(doc, "/users/1/name")).to eq("grace")
      expect(NOSJ.at_pointer(doc, "/a~1b")).to eq(1)
      expect(NOSJ.at_pointer(doc, "/m~0n")).to eq({"deep" => nil})
      expect(NOSJ.at_pointer(doc, "")).to eq(JSON.parse(doc))
    end

    it "returns nil for misses" do
      expect(NOSJ.at_pointer(doc, "/nope")).to be_nil
      expect(NOSJ.at_pointer(doc, "/users/01")).to be_nil
      expect(NOSJ.at_pointer(doc, "/users/-")).to be_nil
    end

    it "raises ArgumentError for malformed pointers" do
      expect { NOSJ.at_pointer(doc, "no-slash") }.to raise_error(ArgumentError)
    end
  end

  it "extracted values match a full parse on a real document" do
    twitter = File.read(File.expand_path("../benchmark/twitter.json", __dir__))
    full = JSON.parse(twitter)
    expect(NOSJ.dig(twitter, "statuses", 95, "user", "screen_name"))
      .to eq(full.dig("statuses", 95, "user", "screen_name"))
    expect(NOSJ.at_pointer(twitter, "/statuses/0/id"))
      .to eq(full.dig("statuses", 0, "id"))
    expect(NOSJ.dig(twitter, "search_metadata"))
      .to eq(full["search_metadata"])
  end
end
