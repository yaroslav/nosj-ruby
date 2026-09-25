# frozen_string_literal: true

# Hostile callbacks: user code (to_json, to_s, respond_to?, exception
# constructors, autoloads) running while the extension holds raw Ruby
# state. Every case runs in a subprocess: a regression here is a
# segfault or heap corruption that must fail one example, not take the
# whole suite down, and several cases patch global classes.
RSpec.describe "memory safety under hostile callbacks" do
  def run_script(script)
    out = IO.popen(
      [RbConfig.ruby, "-I", File.expand_path("../lib", __dir__), "-e", script],
      err: [:child, :out], &:read
    )
    [$?.success?, out]
  end

  def expect_ok(script)
    ok, out = run_script(script)
    expect(ok).to be(true), out
    expect(out).to include("ALL-OK"), out
  end

  # A Ruby exception that longjmps over the generator's Rust frames
  # skips handing the per-thread scratch back, leaking its warm output
  # buffer (~4 MB here) on every raise. Protected calls return it. The
  # script body must define `hostile_call`, which raises through the
  # generator. Control rounds (large generate only) run first so the
  # allocator's retention of the large outputs has settled; the hostile
  # rounds that follow must then stay flat, where a leak adds ~160 MB
  # (no RSS probe on Windows).
  def leak_check
    <<~RUBY
      def rss_mb
        kb = if File.exist?("/proc/self/status")
          File.read("/proc/self/status")[/VmRSS:\\s+(\\d+)/, 1].to_i
        else
          `ps -o rss= -p \#{Process.pid}`.to_i
        end
        kb / 1024.0
      end
      unless Gem.win_platform?
        big = Array.new(200_000) { "xxxxxxxxxxxxxxxx" }
        control = proc { NOSJ.generate(big) }
        hostile = proc { NOSJ.generate(big); begin; hostile_call; rescue StandardError; end }
        40.times(&control)
        GC.start
        before = rss_mb
        40.times(&hostile)
        GC.start
        growth = rss_mb - before
        raise "leaked \#{growth.round} MB across 40 raising calls" if growth > 40
      end
    RUBY
  end

  describe "arrays mutated during generation" do
    it "stops at the live length when a callback shrinks the array" do
      expect_ok(<<~RUBY)
        require "nosj"
        require "json"
        class Emptier
          def initialize(a) = @a = a
          def to_json(*) = (@a.clear; GC.start; '"x"')
        end
        build = -> { a = []; a << Emptier.new(a); a.concat((1..300).map { "e\#{_1}" }); a }
        20.times do
          raise "diverged" unless NOSJ.generate(build.call) == JSON.generate(build.call)
        end
        raise "unexpected" unless NOSJ.generate(build.call) == '["x"]'
        puts "ALL-OK"
      RUBY
    end

    it "emits elements a callback appends, like the json gem" do
      expect_ok(<<~RUBY)
        require "nosj"
        require "json"
        class Grower
          def initialize(a) = @a = a
          def to_json(*) = (@a.push(7, "late"); '"g"')
        end
        build = -> { a = [1]; a << Grower.new(a); a << 2; a }
        raise "diverged" unless NOSJ.generate(build.call) == JSON.generate(build.call)
        raise "unexpected" unless NOSJ.generate(build.call) == '[1,"g",2,7,"late"]'
        puts "ALL-OK"
      RUBY
    end
  end

  describe "raising respond_to? during the to_json fallback" do
    it "propagates the exception without leaking the generate scratch" do
      expect_ok(<<~RUBY)
        require "nosj"
        class RespondBoom; def respond_to?(*) = raise("boom in respond_to?"); end
        def hostile_call = NOSJ.generate([RespondBoom.new])
        begin
          hostile_call
          raise "no exception"
        rescue RuntimeError => e
          raise "wrong exception: \#{e.message}" unless e.message == "boom in respond_to?"
        end
        raise "broken after" unless NOSJ.generate({"a" => [1]}) == '{"a":[1]}'
        #{leak_check}
        puts "ALL-OK"
      RUBY
    end

    it "propagates a raising respond_to_missing? from inside a hash" do
      expect_ok(<<~RUBY)
        require "nosj"
        class MissingBoom; def respond_to_missing?(*) = raise("boom in respond_to_missing?"); end
        def hostile_call = NOSJ.generate({"k" => MissingBoom.new})
        begin
          hostile_call
          raise "no exception"
        rescue RuntimeError => e
          raise "wrong exception: \#{e.message}" unless e.message == "boom in respond_to_missing?"
        end
        #{leak_check}
        puts "ALL-OK"
      RUBY
    end
  end

  describe "raising Exception#to_s on the GeneratorError path" do
    it "propagates the exception without leaking the generate scratch" do
      expect_ok(<<~RUBY)
        require "nosj"
        # BINARY high bytes fail UTF-8 conversion; the generator wraps
        # the conversion error's message into GeneratorError.
        class Encoding::UndefinedConversionError
          def to_s = raise(ArgumentError, "boom in to_s")
        end
        def hostile_call = NOSJ.generate(["\\xFF\\xFE".b])
        begin
          hostile_call
          raise "no exception"
        rescue ArgumentError => e
          raise "wrong exception: \#{e.message}" unless e.message == "boom in to_s"
        end
        #{leak_check}
        puts "ALL-OK"
      RUBY
    end
  end

  describe "a raising JSON::Fragment autoload" do
    %w[strict rails].each do |mode|
      it "propagates from the #{mode}-mode fragment check without leaking the scratch" do
        expect_ok(<<~RUBY)
          require "tmpdir"
          dir = Dir.mktmpdir
          File.write(File.join(dir, "fragment_boom.rb"), 'raise ArgumentError, "boom in autoload"')
          # nosj never loads the json gem itself, so JSON::Fragment can
          # be an autoload whose file raises on every attempt.
          module JSON; end
          JSON.autoload(:Fragment, File.join(dir, "fragment_boom.rb"))
          require "nosj"
          def hostile_call
            #{(mode == "strict") ? "NOSJ.generate([Object.new], strict: true)" : "NOSJ.generate_rails_native([Object.new], true, true)"}
          end
          begin
            hostile_call
            raise "no exception"
          rescue ArgumentError => e
            raise "wrong exception: \#{e.message}" unless e.message == "boom in autoload"
          end
          #{leak_check}
          puts "ALL-OK"
        RUBY
      end
    end
  end

  describe "NOSJ.lazy over a frozen source" do
    # Deduplicating a frozen String subclass (or one carrying an ivar)
    # swaps its heap buffer and frees the old one; lazy nodes borrow
    # frozen sources zero-copy, so they must follow the swap.
    %w[subclass ivar].each do |flavor|
      it "keeps reading the live buffer after -str on a frozen #{flavor} string" do
        expect_ok(<<~RUBY)
          require "nosj"
          payload = "v" * 4000
          10.times do
            text = %({"a": [1, 2, 3], "k": "\#{payload}", "z": 9}) + ""
            src = #{(flavor == "subclass") ? "Class.new(String).new(text)" : "text.tap { _1.instance_variable_set(:@tag, 1) }"}
            src.freeze
            text = nil
            doc = NOSJ.lazy(src)
            -src
            GC.start
            $churn = Array.new(3000) { "Q" * 4100 }
            raise "corrupted k" unless doc["k"] == payload
            raise "corrupted z" unless doc["z"] == 9
            raise "corrupted a" unless doc["a"].to_a == [1, 2, 3]
          end
          puts "ALL-OK"
        RUBY
      end
    end
  end

  describe "NOSJ.splice with values whose to_json misbehaves" do
    # Each value's to_json attacks what splice holds while generating:
    # the source bytes, or the only Ruby reference to a later value.
    # After the attack, GC frees what lost its references, and live
    # churn then reuses both freed object slots ("RRR" strings) and
    # freed buffers ("QQQ..."), so a stale read shows up in the output.
    def splice_script(attack, source_setup)
      <<~RUBY
        require "nosj"
        PAD = "p" * 5000
        EXPECTED = %({"a": 1, "b": "\#{PAD}", "c": 3})
        class Hostile
          def initialize(&attack) = @attack = attack
          def to_json(*)
            @attack.call
            GC.start
            $churn = [Array.new(3000) { "Q" * 5100 }, Array.new(20_000) { "R" * 3 }]
            "1"
          end
        end
        5.times do
          #{source_setup}
          edits = {}
          edits["/a"] = Hostile.new { #{attack} }
          edits["/c"] = Object.new.tap { |o| def o.to_json(*) = "3" }
          out = NOSJ.splice(src, edits)
          raise "corrupted: \#{out[0, 40].inspect}" unless out == EXPECTED
        end
        puts "ALL-OK"
      RUBY
    end

    it "is unaffected by a callback that clears the edits hash" do
      expect_ok(splice_script("edits.clear", "src = EXPECTED.dup"))
    end

    it "never reads a source buffer a callback reallocated" do
      ok, out = run_script(<<~RUBY)
        require "nosj"
        pad = "p" * 5000
        src = %({"a": 1, "b": "\#{pad}", "c": 3})
        attack = Object.new
        attack.define_singleton_method(:to_json) do |*|
          src.replace(%({"a": 0, "c": 0}))
          Array.new(3000) { "Q" * 5100 }
          GC.start
          "1"
        end
        out = NOSJ.splice(src, "/a" => attack, "/c" => 3)
        raise "leaked heap bytes: \#{out[0, 40].inspect}" if out.include?("QQQQ")
        puts out
        puts "ALL-OK"
      RUBY
      expect(ok).to be(true), out
      expect(out).to include("ALL-OK"), out
      # The edits land on the document as it stands after the callbacks.
      expect(out).to include(%({"a": 1, "c": 3}))
    end

    it "survives a frozen String subclass whose buffer is swapped by deduplication" do
      # Built from an unretained temporary: deduplication frees the
      # buffer only when nothing else shares it.
      expect_ok(splice_script("-src", "src = Class.new(String).new(EXPECTED + \"\").freeze"))
    end
  end
end
