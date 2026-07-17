# frozen_string_literal: true

require "tempfile"

RSpec.describe NOSJ::ParserError do
  def error_for(src, opts = nil)
    NOSJ.parse(src, opts)
    raise "expected a parse error for #{src.inspect}"
  rescue described_class => e
    e
  end

  it "carries byte_offset, line, and column for the failure" do
    src = %({\n  "a": 1,\n  "b": }\n)
    e = error_for(src)
    expect(e.byte_offset).to eq(src.index("}"))
    expect(e.line).to eq(3)
    expect(e.column).to eq(8)
  end

  it "counts columns in characters, not bytes" do
    src = %({"héllo→": nope})
    e = error_for(src)
    expect(e.byte_offset).to eq(src.byteindex("nope"))
    expect(e.column).to eq(src.index("nope") + 1)
  end

  it "renders a caret snippet under the offending character" do
    e = error_for(%(  {"a": nope}))
    content, caret = e.snippet.lines.map(&:chomp)
    expect(content).to eq(%(  {"a": nope}))
    expect(caret).to eq("#{" " * content.index("nope")}^")
  end

  it "windows the snippet on long minified lines" do
    long = "{" + (1..600).map { |i| %("k#{i}":#{i}) }.join(",") + ",}"
    e = error_for(long)
    content, caret = e.snippet.lines.map(&:chomp)
    expect(content.length).to be < 100
    expect(content).to start_with("…")
    expect(content[caret.index("^")]).to eq("}")
    expect(e.line).to eq(1)
    expect(e.column).to eq(long.length)
  end

  it "positions unexpected end of input just past the source" do
    e = error_for("[1, 2")
    expect(e.byte_offset).to eq(5)
    expect(e.column).to eq(6)
    expect(e.snippet.lines.last).to eq("#{" " * 5}^")
  end

  it "leaves the position nil when there is none (encoding refusals)" do
    e = error_for("[1]".encode(Encoding::UTF_16LE))
    expect(e.byte_offset).to be_nil
    expect(e.line).to be_nil
    expect(e.column).to be_nil
    expect(e.snippet).to be_nil
    expect(e.detailed_message).to include(e.message)
  end

  it "appends the snippet to detailed_message" do
    e = error_for('{"a": nope}')
    expect(e.detailed_message).to include(e.message)
    expect(e.detailed_message).to include(e.snippet)
    expect(e.detailed_message(highlight: true)).to include(e.snippet)
  end

  describe "absolute positions through partial parsing" do
    let(:src) { %({"pad": [1, 2, 3], "a": {"deep": nope}}) }

    it "reports document offsets from dig/at_pointer" do
      %w[dig at_pointer].each do |entry|
        e = begin
          (entry == "dig") ? NOSJ.dig(src, "a") : NOSJ.at_pointer(src, "/a")
          raise "expected a parse error"
        rescue described_class => err
          err
        end
        expect(e.byte_offset).to eq(src.index("nope")), entry
        expect(e.snippet).to include('{"deep": nope}'), entry
      end
    end

    it "reports document offsets from lazy access" do
      node = NOSJ.lazy(src)["a"]
      e = begin
        node.value
        raise "expected a parse error"
      rescue described_class => err
        err
      end
      expect(e.byte_offset).to eq(src.index("nope"))
      expect(e.column).to eq(src.index("nope") + 1)
    end
  end

  it "positions errors from file parsing" do
    Tempfile.create(["rich", ".json"]) do |f|
      f.write(%({\n  "broken": \n}))
      f.flush
      e = begin
        NOSJ.load_file(f.path)
        raise "expected a parse error"
      rescue described_class => err
        err
      end
      expect(e.line).to eq(3)
    end
  end
end
