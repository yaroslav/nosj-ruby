# frozen_string_literal: true

require "json"

RSpec.describe "splice / JSON Patch / merge patch" do
  describe "NOSJ.splice" do
    it "replaces one value, leaving every other byte untouched" do
      json = %({\n  "config": { "timeout": 10, "retries": 3 },\n  "n": 1.50\n})
      out = NOSJ.splice(json, "/config/timeout" => 30)
      expect(out).to eq(json.sub("10", "30"))
      # Formatting and the noncanonical 1.50 spelling survive.
      expect(out).to include(%("n": 1.50))
    end

    it "applies batches in one pass, sorted by position" do
      out = NOSJ.splice(%({"a":1,"b":[1,2],"c":3}),
        {"/c" => "z", "/a" => {"x" => 1}, "/b/1" => nil})
      expect(out).to eq(%({"a":{"x":1},"b":[1,null],"c":"z"}))
    end

    it "generates replacement values byte-identically to generate" do
      value = {"esc" => %(<a "quote" \\ \n>), "f" => [2.34387207031, 1.0 / 3.0]}
      out = NOSJ.splice(%({"k":0}), "/k" => value)
      expect(out).to eq(%({"k":#{NOSJ.generate(value)}}))
      expect(NOSJ.parse(out)["k"]).to eq(NOSJ.parse(NOSJ.generate(value)))
    end

    it "honors generate options for inserted values" do
      out = NOSJ.splice(%({"k":0}), {"/k" => "héllo"}, ascii_only: true)
      expect(out).to eq(%({"k":"h\\u00e9llo"}))
    end

    it "resolves escaped keys and the root pointer" do
      expect(NOSJ.splice(%({"a/b":1,"c~d":2}), "/a~1b" => 3, "/c~0d" => 4))
        .to eq(%({"a/b":3,"c~d":4}))
      expect(NOSJ.splice(%({"old":1}), "" => [1, 2])).to eq("[1,2]")
    end

    it "raises KeyError for missing targets, ArgumentError for conflicts" do
      expect { NOSJ.splice(%({"a":1}), "/nope" => 2) }
        .to raise_error(KeyError, %r{/nope})
      expect { NOSJ.splice(%({"a":{"b":1}}), {"/a" => 1, "/a/b" => 2}) }
        .to raise_error(ArgumentError, /overlap/)
      expect { NOSJ.splice(%({"a":1}), "no-slash" => 1) }
        .to raise_error(ArgumentError)
      expect { NOSJ.splice(%({"a":1}), 42 => 1) }
        .to raise_error(ArgumentError, /Strings/)
    end

    it "raises rich ParserErrors on malformed documents" do
      NOSJ.splice(%({"a": nope}), "/a" => 1)
      raise "expected a parse error"
    rescue NOSJ::ParserError => e
      expect(e.byte_offset).to eq(6)
    end

    it "returns a copy for an empty edit set" do
      src = %({"a":1})
      out = NOSJ.splice(src, {})
      expect(out).to eq(src)
      expect(out).not_to equal(src)
    end
  end

  describe "NOSJ.patch (RFC 6902 appendix A)" do
    def apply(doc, patch)
      NOSJ.parse(NOSJ.patch(NOSJ.generate(doc), patch))
    end

    it "A.1 adds an object member" do
      expect(apply({"foo" => "bar"}, [{"op" => "add", "path" => "/baz", "value" => "qux"}]))
        .to eq({"baz" => "qux", "foo" => "bar"})
    end

    it "A.2 adds an array element" do
      expect(apply({"foo" => %w[bar baz]}, [{"op" => "add", "path" => "/foo/1", "value" => "qux"}]))
        .to eq({"foo" => %w[bar qux baz]})
    end

    it "A.3 removes an object member" do
      expect(apply({"baz" => "qux", "foo" => "bar"}, [{"op" => "remove", "path" => "/baz"}]))
        .to eq({"foo" => "bar"})
    end

    it "A.4 removes an array element" do
      expect(apply({"foo" => %w[bar qux baz]}, [{"op" => "remove", "path" => "/foo/1"}]))
        .to eq({"foo" => %w[bar baz]})
    end

    it "A.5 replaces a value" do
      expect(apply({"baz" => "qux", "foo" => "bar"},
        [{"op" => "replace", "path" => "/baz", "value" => "boo"}]))
        .to eq({"baz" => "boo", "foo" => "bar"})
    end

    it "A.6 moves a value" do
      doc = {"foo" => {"bar" => "baz", "waldo" => "fred"}, "qux" => {"corge" => "grault"}}
      expect(apply(doc, [{"op" => "move", "from" => "/foo/waldo", "path" => "/qux/thud"}]))
        .to eq({"foo" => {"bar" => "baz"}, "qux" => {"corge" => "grault", "thud" => "fred"}})
    end

    it "A.7 moves an array element" do
      expect(apply({"foo" => %w[all grass cows eat]},
        [{"op" => "move", "from" => "/foo/1", "path" => "/foo/3"}]))
        .to eq({"foo" => %w[all cows eat grass]})
    end

    it "A.8 passes a successful test" do
      doc = {"baz" => "qux", "foo" => %w[a 2 c]}
      patch = [
        {"op" => "test", "path" => "/baz", "value" => "qux"},
        {"op" => "test", "path" => "/foo/1", "value" => "2"}
      ]
      expect(apply(doc, patch)).to eq(doc)
    end

    it "A.9 fails a test" do
      expect { apply({"baz" => "qux"}, [{"op" => "test", "path" => "/baz", "value" => "bar"}]) }
        .to raise_error(NOSJ::PatchError, /test failed/)
    end

    it "A.10 adds a nested member object" do
      expect(apply({"foo" => "bar"},
        [{"op" => "add", "path" => "/child", "value" => {"grandchild" => {}}}]))
        .to eq({"foo" => "bar", "child" => {"grandchild" => {}}})
    end

    it "A.11 ignores unrecognized op members" do
      expect(apply({"foo" => "bar"},
        [{"op" => "add", "path" => "/baz", "value" => "qux", "xyz" => 123}]))
        .to eq({"foo" => "bar", "baz" => "qux"})
    end

    it "A.12 errors adding to a nonexistent target" do
      expect { apply({"foo" => "bar"}, [{"op" => "add", "path" => "/baz/bat", "value" => "qux"}]) }
        .to raise_error(NOSJ::PatchError, /does not exist/)
    end

    it "A.14 evaluates ~ escape ordering" do
      expect(apply({"/" => 9, "~1" => 10}, [{"op" => "test", "path" => "/~01", "value" => 10}]))
        .to eq({"/" => 9, "~1" => 10})
    end

    it "A.15 does not equate strings and numbers" do
      expect { apply({"/" => 9, "~1" => 10}, [{"op" => "test", "path" => "/~01", "value" => "10"}]) }
        .to raise_error(NOSJ::PatchError)
    end

    it "A.16 adds an array value as one element" do
      expect(apply({"foo" => ["bar"]},
        [{"op" => "add", "path" => "/foo/-", "value" => %w[abc def]}]))
        .to eq({"foo" => ["bar", %w[abc def]]})
    end
  end

  describe "NOSJ.patch semantics" do
    it "applies ops sequentially, each seeing the previous result" do
      out = NOSJ.patch(%({"a":1}), [
        {"op" => "add", "path" => "/b", "value" => [1]},
        {"op" => "add", "path" => "/b/-", "value" => 2},
        {"op" => "test", "path" => "/b", "value" => [1, 2]},
        {"op" => "remove", "path" => "/a"}
      ])
      expect(NOSJ.parse(out)).to eq({"b" => [1, 2]})
    end

    it "accepts Symbol keys and null values" do
      expect(NOSJ.patch(%({"a":1}), [{op: "add", path: "/n", value: nil}]))
        .to eq(%({"a":1,"n":null}))
    end

    it "add replaces existing object members but inserts into arrays" do
      expect(NOSJ.patch(%({"a":1}), [{"op" => "add", "path" => "/a", "value" => 2}]))
        .to eq(%({"a":2}))
      expect(NOSJ.patch("[1,2]", [{"op" => "add", "path" => "/1", "value" => 9}]))
        .to eq("[1,9,2]")
    end

    it "add on the root replaces the whole document" do
      expect(NOSJ.patch(%({"a":1}), [{"op" => "add", "path" => "", "value" => [true]}]))
        .to eq("[true]")
    end

    it "edits only the touched spans" do
      json = %({ "keep": 1.50,\n  "list": [ 1, 2, 3 ] })
      out = NOSJ.patch(json, [{"op" => "remove", "path" => "/list/1"}])
      expect(out).to eq(%({ "keep": 1.50,\n  "list": [ 1, 3 ] }))
    end

    it "raises PatchError for application failures" do
      doc = %({"a":{"b":1},"list":[1]})
      [
        [{"op" => "replace", "path" => "/nope", "value" => 1}, /does not exist/],
        [{"op" => "remove", "path" => "/nope"}, /does not exist/],
        [{"op" => "remove", "path" => ""}, /root/],
        [{"op" => "move", "from" => "/a", "path" => "/a/b"}, /own child/],
        [{"op" => "add", "path" => "/list/7", "value" => 1}, /out of range/],
        [{"op" => "add", "path" => "/list/01", "value" => 1}, /invalid array index/],
        [{"op" => "copy", "from" => "/nope", "path" => "/x"}, /does not exist/]
      ].each do |op, msg|
        expect { NOSJ.patch(doc, [op]) }.to raise_error(NOSJ::PatchError, msg), op.inspect
      end
    end

    it "raises ArgumentError for malformed patch documents" do
      [
        [[42], /not a Hash/],
        [[{}], /missing "op"/],
        [[{"op" => "add"}], /missing "path"/],
        [[{"op" => "add", "path" => "/a"}], /missing "value"/],
        [[{"op" => "move", "path" => "/a"}], /missing "from"/],
        [[{"op" => "frobnicate", "path" => "/a"}], /unknown patch op/]
      ].each do |ops, msg|
        expect { NOSJ.patch(%({"a":1}), ops) }.to raise_error(ArgumentError, msg), ops.inspect
      end
    end

    it "move onto itself is a no-op" do
      expect(NOSJ.patch(%({"a":1}), [{"op" => "move", "from" => "/a", "path" => "/a"}]))
        .to eq(%({"a":1}))
    end

    it "empties and refills containers cleanly" do
      expect(NOSJ.patch(%({"a": 1 }), [{"op" => "remove", "path" => "/a"}])).to eq("{}")
      expect(NOSJ.patch("[ 1 ]", [{"op" => "remove", "path" => "/0"}])).to eq("[]")
      expect(NOSJ.patch("{}", [{"op" => "add", "path" => "/a", "value" => 1}])).to eq(%({"a":1}))
      expect(NOSJ.patch("[]", [{"op" => "add", "path" => "/-", "value" => 1}])).to eq("[1]")
      expect(NOSJ.patch("[]", [{"op" => "add", "path" => "/0", "value" => 1}])).to eq("[1]")
    end

    it "round-trips patched corpus documents as valid JSON" do
      src = File.read(File.join(__dir__, "../benchmark/twitter.json"))
      out = NOSJ.patch(src, [
        {"op" => "replace", "path" => "/search_metadata/count", "value" => 42},
        {"op" => "add", "path" => "/statuses/0/patched", "value" => true},
        {"op" => "remove", "path" => "/statuses/99"}
      ])
      parsed = NOSJ.parse(out)
      expect(parsed["search_metadata"]["count"]).to eq(42)
      expect(parsed["statuses"][0]["patched"]).to be(true)
      expect(parsed["statuses"].size).to eq(99)
      # Everything else survives byte-for-byte semantics.
      reference = JSON.parse(src)
      reference["search_metadata"]["count"] = 42
      reference["statuses"][0]["patched"] = true
      reference["statuses"].delete_at(99)
      expect(parsed).to eq(reference)
    end
  end

  describe "NOSJ.merge_patch (RFC 7386 appendix)" do
    it "matches every RFC test case" do
      # The RFC's full test-case table: [target, patch, expected].
      cases = [
        [%({"a":"b"}), {"a" => "c"}, {"a" => "c"}],
        [%({"a":"b"}), {"b" => "c"}, {"a" => "b", "b" => "c"}],
        [%({"a":"b"}), {"a" => nil}, {}],
        [%({"a":"b","b":"c"}), {"a" => nil}, {"b" => "c"}],
        [%({"a":["b"]}), {"a" => "c"}, {"a" => "c"}],
        [%({"a":"c"}), {"a" => ["b"]}, {"a" => ["b"]}],
        [%({"a":{"b":"c"}}), {"a" => {"b" => "d", "c" => nil}}, {"a" => {"b" => "d"}}],
        [%({"a":[{"b":"c"}]}), {"a" => [1]}, {"a" => [1]}],
        [%(["a","b"]), %w[c d], %w[c d]],
        [%({"a":"b"}), ["c"], ["c"]],
        [%({"a":"foo"}), nil, nil],
        [%({"a":"foo"}), "bar", "bar"],
        [%({"e":null}), {"a" => 1}, {"e" => nil, "a" => 1}],
        [%([1,2]), {"a" => "b", "c" => nil}, {"a" => "b"}],
        [%({}), {"a" => {"bb" => {"ccc" => nil}}}, {"a" => {"bb" => {}}}]
      ]
      cases.each_with_index do |(target, patch, expected), i|
        out = NOSJ.merge_patch(target, patch)
        expect(NOSJ.parse(out)).to eq(expected), "case #{i}: #{target} + #{patch.inspect}"
      end
    end

    it "matches Symbol patch keys against String document keys" do
      expect(NOSJ.merge_patch(%({"a":{"b":1,"c":2}}), {a: {b: nil, d: 3}}))
        .to eq(%({"a":{"c":2,"d":3}}))
    end

    it "honors generate options" do
      expect(NOSJ.merge_patch(%({}), {"k" => "é"}, ascii_only: true))
        .to eq(%({"k":"\\u00e9"}))
    end
  end
end
