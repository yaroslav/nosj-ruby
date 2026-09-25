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
