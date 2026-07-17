# frozen_string_literal: true

require "json"

RSpec.describe "NOSJ.lazy" do
  let(:doc) do
    <<~JSON
      {
        "users": [
          {"name": "ada", "tags": ["math", "engines"]},
          {"name": "grace", "score": 99.5, "active": true}
        ],
        "a/b": 1,
        "m~n": {"deep": null},
        "count": 12345678901234567890,
        "empty": {}
      }
    JSON
  end

  describe "creation" do
    it "returns a Lazy node for container roots" do
      expect(NOSJ.lazy(doc)).to be_a(NOSJ::Lazy)
      expect(NOSJ.lazy("[1,2]")).to be_a(NOSJ::Lazy)
    end

    it "materializes scalar roots immediately" do
      expect(NOSJ.lazy("123")).to eq(123)
      expect(NOSJ.lazy('"hi"')).to eq("hi")
      expect(NOSJ.lazy("null")).to be_nil
    end

    it "rejects broken and non-UTF-8 input like parse" do
      expect { NOSJ.lazy("\xFF\xFE".dup.force_encoding(Encoding::UTF_8)) }
        .to raise_error(NOSJ::ParserError)
      expect { NOSJ.lazy("{}".encode(Encoding::UTF_16LE)) }
        .to raise_error(NOSJ::ParserError)
    end

    it "rejects malformed roots eagerly" do
      expect { NOSJ.lazy("") }.to raise_error(NOSJ::ParserError)
      expect { NOSJ.lazy("nope") }.to raise_error(NOSJ::ParserError)
    end

    it "cannot be allocated directly" do
      expect { NOSJ::Lazy.new }.to raise_error(TypeError, /allocator undefined/)
    end
  end

  describe "#[]" do
    let(:lazy) { NOSJ.lazy(doc) }

    it "returns lazy nodes for containers and values for scalars" do
      expect(lazy["users"]).to be_a(NOSJ::Lazy)
      expect(lazy["users"][0]).to be_a(NOSJ::Lazy)
      expect(lazy["users"][0]["name"]).to eq("ada")
      expect(lazy["users"][1]["score"]).to eq(99.5)
      expect(lazy["users"][1]["active"]).to be(true)
      expect(lazy["m~n"]["deep"]).to be_nil
      expect(lazy["count"]).to eq(12_345_678_901_234_567_890)
    end

    it "accepts Symbol keys" do
      expect(lazy[:users][0][:name]).to eq("ada")
    end

    it "returns nil for misses and negative indices" do
      expect(lazy["nope"]).to be_nil
      expect(lazy["users"][5]).to be_nil
      expect(lazy["users"][-1]).to be_nil
    end

    it "caches children, keeping identity stable" do
      expect(lazy["users"]).to equal(lazy["users"])
    end

    it "raises for unsupported key types" do
      expect { lazy[1.5] }.to raise_error(ArgumentError)
    end

    it "defers malformed-interior errors until the content is parsed" do
      # Skipping checks bracket balance only (crate semantics), so the
      # broken scalar inside the array surfaces at materialization.
      broken = NOSJ.lazy('{"a": [1, tru], "b": 2}')
      node = broken["a"]
      expect(node).to be_a(NOSJ::Lazy)
      expect { node.value }.to raise_error(NOSJ::ParserError)
      expect(broken["b"]).to eq(2)
    end
  end

  describe "#dig and #at_pointer" do
    let(:lazy) { NOSJ.lazy(doc) }

    it "digs whole paths in one resolution, matching a full parse" do
      parsed = JSON.parse(doc)
      expect(lazy.dig("users", 0, "tags", 1)).to eq(parsed.dig("users", 0, "tags", 1))
      expect(lazy.dig("users", 1)).to be_a(NOSJ::Lazy)
      expect(lazy.dig(:users, 0, :name)).to eq("ada")
      expect(lazy.dig("users", 9, "name")).to be_nil
      expect(lazy.dig("nope", "deeper")).to be_nil
    end

    it "matches NOSJ.dig semantics for scalar steps and negative indices" do
      expect(lazy.dig("a/b", "deeper")).to eq(NOSJ.dig(doc, "a/b", "deeper"))
      expect(lazy.dig("a/b", "deeper")).to be_nil
      expect(lazy.dig("users", -1, "name")).to be_nil
    end

    it "resolves RFC 6901 pointers, including escapes, within any node" do
      expect(lazy.at_pointer("/users/1/name")).to eq("grace")
      expect(lazy.at_pointer("/a~1b")).to eq(1)
      expect(lazy.at_pointer("/m~0n/deep")).to be_nil
      expect(lazy["users"].at_pointer("/0/tags/0")).to eq("math")
      expect { lazy.at_pointer("users") }.to raise_error(ArgumentError)
    end
  end

  describe "#value" do
    let(:lazy) { NOSJ.lazy(doc) }

    it "materializes subtrees byte-identically to a full parse" do
      parsed = JSON.parse(doc)
      expect(lazy.value).to eq(parsed)
      expect(lazy["users"].value).to eq(parsed["users"])
      expect(lazy["users"][1].to_h).to eq(parsed["users"][1])
      expect(lazy["users"][0]["tags"].to_a).to eq(parsed["users"][0]["tags"])
    end

    it "honors parse options given to NOSJ.lazy" do
      sym = NOSJ.lazy(doc, symbolize_names: true)
      expect(sym["users"][0].value).to eq({name: "ada", tags: %w[math engines]})

      frozen = NOSJ.lazy(doc, freeze: true)
      value = frozen["users"][0].value
      expect(value).to be_frozen
      expect(value["name"]).to be_frozen
    end

    it "type-checks to_h and to_a" do
      expect { lazy.to_a }.to raise_error(TypeError)
      expect { lazy["users"].to_h }.to raise_error(TypeError)
    end
  end

  describe "enumeration" do
    let(:lazy) { NOSJ.lazy(doc) }

    it "reads keys and size without materializing values" do
      expect(lazy.keys).to eq(JSON.parse(doc).keys)
      expect(lazy.size).to eq(5)
      expect(lazy["users"].size).to eq(2)
      expect(lazy["users"][0]["tags"].length).to eq(2)
      expect(lazy["empty"]).to be_empty
      expect { lazy["users"].keys }.to raise_error(TypeError)
    end

    it "iterates object nodes as [key, child] pairs" do
      pairs = lazy["users"][1].each.to_a
      expect(pairs.map(&:first)).to eq(%w[name score active])
      expect(pairs.map(&:last)).to eq(["grace", 99.5, true])
    end

    it "iterates array nodes as children, containers staying lazy" do
      children = lazy["users"].to_enum(:each).to_a
      expect(children).to all(be_a(NOSJ::Lazy))
      expect(children.map { |c| c["name"] }).to eq(%w[ada grace])
    end

    it "supports Enumerable" do
      expect(lazy["users"][0]["tags"].map(&:upcase)).to eq(%w[MATH ENGINES])
    end
  end

  describe "#inspect" do
    it "shows kind and span size without dumping content" do
      lazy = NOSJ.lazy(doc)
      expect(lazy.inspect).to match(/\A#<NOSJ::Lazy object \(\d+ bytes\)>\z/)
      expect(lazy["users"].inspect).to match(/\A#<NOSJ::Lazy array \(\d+ bytes\)>\z/)
    end
  end

  describe "document lifetime" do
    it "survives mutation and GC of the source string" do
      src = +'{"a":{"b":[1,2,3]}}'
      lazy = NOSJ.lazy(src)
      src.replace("X" * 64)
      src = nil # rubocop:disable Lint/UselessAssignment
      GC.start
      expect(lazy["a"]["b"].value).to eq([1, 2, 3])
    end

    it "borrows frozen sources safely across GC and compaction" do
      # Literals are frozen here (magic comment), so this takes the
      # zero-copy borrow path.
      lazy = NOSJ.lazy('{"a":{"b":[1,2,3]}}')
      GC.start
      GC.compact if GC.respond_to?(:compact)
      expect(lazy["a"]["b"].value).to eq([1, 2, 3])
      expect(lazy.value).to eq({"a" => {"b" => [1, 2, 3]}})
    end
  end

  describe "corpus equivalence" do
    it "matches full parses on benchmark files" do
      corpus_files.first(3).each do |path|
        json = File.read(path)
        parsed = NOSJ.parse(json)
        lazy = NOSJ.lazy(json)
        next expect(lazy).to eq(parsed) unless lazy.is_a?(NOSJ::Lazy)

        expect(lazy.value).to eq(parsed)
        if parsed.is_a?(Hash)
          expect(lazy.keys).to eq(parsed.keys)
          key = parsed.keys.first
          expect(lazy[key].is_a?(NOSJ::Lazy) ? lazy[key].value : lazy[key]).to eq(parsed[key])
        end
      end
    end
  end
end
