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
end
