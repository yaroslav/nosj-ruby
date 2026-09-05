# frozen_string_literal: true

require "json"
require "tmpdir"

# Ractor support targets Ruby 4.0+ (Ractor#value and the Port API); the
# extension declares Ractor safety on every Ruby it builds for, but these
# examples use the 4.0 API.
if Gem::Version.new(RUBY_VERSION) >= Gem::Version.new("4.0")
  Warning[:experimental] = false

  # A value whose to_json re-enters the generator: the reentrancy case,
  # where a recursive generate on the same Ruby thread finds the thread's
  # scratch already taken.
  class RactorSpecTag
    def initialize(name)
      @name = name
    end

    def to_json(*)
      NOSJ.generate({"tag" => @name, "nested" => [1, 2.5, nil]})
    end
  end

  # Ractor blocks cannot reach the example (self, lets, locals), so the
  # workloads live in a module both sides call with plain arguments.
  module RactorSpecBattery
    module_function

    def reentrant
      NOSJ.generate([RactorSpecTag.new("x"), {"k" => RactorSpecTag.new("y")}])
    end

    def entry_points(doc)
      [
        NOSJ.valid?(doc),
        NOSJ.dig(doc, "statuses", 0, "id"),
        NOSJ.at_pointer(doc, "/statuses/1/user/name"),
        NOSJ.at_pointers(doc, ["/statuses/0/id", "/missing"]),
        NOSJ.dig_many(doc, [["statuses", 2, "id"], ["nope"]]),
        NOSJ.stats(doc)[:values],
        NOSJ.each_line(%([1]\n{"a":2}\n)).to_a,
        NOSJ.generate_lines([1, {"a" => 2}]),
        NOSJ.splice(%({"a":1,"b":[1,2]}), "/b/1" => 9),
        NOSJ.patch(%({"a":1}), [{"op" => "add", "path" => "/b", "value" => [true]}]),
        NOSJ.merge_patch(%({"a":1,"b":2}), {"b" => nil, "c" => 3}),
        NOSJ.minify(doc).bytesize,
        NOSJ.reformat(doc, pretty: true).bytesize
      ]
    end

    def lazy(doc)
      lazy = NOSJ.lazy(doc.dup.freeze)
      statuses = lazy["statuses"]
      [
        statuses[0]["user"]["name"],
        lazy.keys,
        lazy.size,
        statuses.dig(1, "id"),
        statuses.map { |s| s["id"] }.first(3),
        statuses[2].to_h.keys.sort
      ]
    end

    def files(dir, doc)
      path = File.join(dir, "doc.json")
      File.write(path, doc)
      out = File.join(dir, "out.json")
      lines = File.join(dir, "out.ndjson")
      [
        NOSJ.load_file(path) == NOSJ.parse(doc),
        NOSJ.dig_file(path, "statuses", 0, "id"),
        NOSJ.at_pointer_file(path, "/statuses/0/id"),
        NOSJ.load_lazy_file(path)["statuses"].size,
        NOSJ.stats_file(path)[:byte_size],
        NOSJ.reformat_file(path, pretty: true).bytesize,
        NOSJ.write_file(out, NOSJ.parse(doc)),
        NOSJ.write_lines(lines, [1, [2]]),
        NOSJ.each_line_file(lines).to_a
      ]
    end

    def rich_error(source)
      NOSJ.parse(source)
    rescue NOSJ::ParserError => e
      [e.class.name, e.message, e.byte_offset, e.line, e.column, e.snippet]
    end
  end

  RSpec.describe "NOSJ inside Ractors" do
    def in_ractor(*args, &block)
      Ractor.new(*args, &block).value
    end

    let(:corpus_path) { File.expand_path("../benchmark/twitter.json", __dir__) }
    let(:doc) { File.read(corpus_path) }

    it "does not raise Ractor::UnsafeError for a native call (declaration canary)" do
      outcome = in_ractor do
        NOSJ.parse("[1]")
      rescue Ractor::UnsafeError => e
        e
      end
      expect(outcome).to eq([1])
    end

    it "parses a nested document like the main Ractor" do
      expect(in_ractor(doc) { |d| NOSJ.parse(d) }).to eq(NOSJ.parse(doc))
      symbolized = in_ractor(doc) { |d| NOSJ.parse(d, symbolize_names: true, freeze: true) }
      expect(symbolized).to eq(NOSJ.parse(doc, symbolize_names: true, freeze: true))
    end

    it "generates like the main Ractor" do
      obj = NOSJ.parse(doc)
      expected = [NOSJ.generate(obj), NOSJ.pretty_generate(obj), NOSJ.generate(obj, ascii_only: true)]
      generated = in_ractor(doc) do |d|
        o = NOSJ.parse(d)
        [NOSJ.generate(o), NOSJ.pretty_generate(o), NOSJ.generate(o, ascii_only: true)]
      end
      expect(generated).to eq(expected)
    end

    it "re-enters the generator from a to_json callback" do
      expect(in_ractor { RactorSpecBattery.reentrant }).to eq(RactorSpecBattery.reentrant)
    end

    it "serves the lazy document API" do
      expect(in_ractor(doc) { |d| RactorSpecBattery.lazy(d) }).to eq(RactorSpecBattery.lazy(doc))
    end

    it "serves the partial, validation, statistics, lines, patch, and reformat entry points" do
      expect(in_ractor(doc) { |d| RactorSpecBattery.entry_points(d) })
        .to eq(RactorSpecBattery.entry_points(doc))
    end

    it "serves the file entry points" do
      Dir.mktmpdir("nosj-ractor") do |dir|
        main_dir = File.join(dir, "main")
        ractor_dir = File.join(dir, "ractor")
        Dir.mkdir(main_dir)
        Dir.mkdir(ractor_dir)
        expect(in_ractor(ractor_dir, doc) { |rd, d| RactorSpecBattery.files(rd, d) })
          .to eq(RactorSpecBattery.files(main_dir, doc))
      end
    end

    it "raises the rich ParserError with the same position info" do
      source = "{\n  \"a\": }"
      expect(in_ractor(source) { |s| RactorSpecBattery.rich_error(s) })
        .to eq(RactorSpecBattery.rich_error(source))
      expect(RactorSpecBattery.rich_error(source).first).to eq("NOSJ::ParserError")
    end

    it "raises generate-side errors as the gem's classes" do
      classes = in_ractor do
        [Float::NAN, [[[1]]]].zip([{}, {max_nesting: 1}]).map do |value, opts|
          NOSJ.generate(value, opts)
          nil
        rescue NOSJ::Error => e
          e.class.name
        end
      end
      expect(classes).to eq(%w[NOSJ::GeneratorError NOSJ::NestingError])
    end

    it "keeps freeze: true output shareable" do
      expect(in_ractor(doc) { |d| Ractor.shareable?(NOSJ.parse(d, freeze: true)) }).to be(true)
    end

    it "runs parse and generate with to_json callbacks in 4 parallel Ractors" do
      expected = NOSJ.generate([NOSJ.parse(doc), RactorSpecTag.new("t")])
      results = Array.new(4) do
        Ractor.new(doc) do |d|
          Array.new(200) { NOSJ.generate([NOSJ.parse(d), RactorSpecTag.new("t")]) }.uniq
        end
      end.map(&:value)
      expect(results).to all(eq([expected]))
    end

    it "runs partial parsing and lazy access in 4 parallel Ractors" do
      expected = [NOSJ.dig(doc, "statuses", 0, "id"), NOSJ.lazy(doc.dup.freeze)["statuses"].size, NOSJ.valid?(doc)]
      results = Array.new(4) do
        Ractor.new(doc) do |d|
          frozen = d.dup.freeze
          Array.new(200) { [NOSJ.dig(d, "statuses", 0, "id"), NOSJ.lazy(frozen)["statuses"].size, NOSJ.valid?(d)] }.uniq
        end
      end.map(&:value)
      expect(results).to all(eq([expected]))
    end

    # The first touch of every lazily-resolved class and ID must not
    # happen inside a Ractor: before the init-time warm-up, four Ractors
    # racing it deadlocked about half the time. A fresh process makes
    # sure the main Ractor never touched nosj first.
    it "survives first touches racing inside Ractors in a fresh process" do
      script = <<~RUBY
        require "nosj"
        Warning[:experimental] = false
        doc = File.read(#{corpus_path.inspect})
        ractors = Array.new(4) do
          Ractor.new(doc) do |d|
            Array.new(50) do
              [NOSJ.dig(d, "statuses", 0, "id"), NOSJ.lazy(d.dup.freeze)["statuses"].size,
                NOSJ.generate(NOSJ.parse(d)).bytesize]
            end.uniq
          end
        end
        results = ractors.map(&:value)
        raise "mismatch: \#{results.inspect}" unless results.uniq.size == 1 && results[0].size == 1
        puts "ALL-OK"
      RUBY
      ok, out = run_with_deadline(script, 120)
      expect(ok).to be(true), out
      expect(out).to include("ALL-OK")
    end

    # A hung child is the failure this guards against, so wait with a
    # deadline and kill on expiry instead of blocking on the pipe.
    def run_with_deadline(script, seconds)
      reader, writer = IO.pipe
      pid = Process.spawn(
        RbConfig.ruby, "-I", File.expand_path("../lib", __dir__), "-e", script,
        out: writer, err: writer
      )
      writer.close
      deadline = Process.clock_gettime(Process::CLOCK_MONOTONIC) + seconds
      loop do
        _, status = Process.wait2(pid, Process::WNOHANG)
        return [status.success?, reader.read] if status
        if Process.clock_gettime(Process::CLOCK_MONOTONIC) > deadline
          Process.kill("KILL", pid)
          Process.wait(pid)
          return [false, "timed out after #{seconds}s: a first touch hung inside a Ractor"]
        end
        sleep 0.05
      end
    ensure
      reader&.close
    end
  end
end
