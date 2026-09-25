# frozen_string_literal: true

# Specs that patch global state, or whose failure mode is a crash, run
# their assertions in a child Ruby with the gem's lib on the load path.
# `expect_ok` passes when the script exits cleanly after printing ALL-OK,
# and returns the child's output.
module SubprocessHelper
  LIB = File.expand_path("../../lib", __dir__)

  def run_script(script)
    out = IO.popen([RbConfig.ruby, "-I", LIB, "-e", script], err: [:child, :out], &:read)
    [$?.success?, out]
  end

  def expect_ok(script)
    ok, out = run_script(script)
    expect(ok).to be(true), out
    expect(out).to include("ALL-OK"), out
    out
  end
end

RSpec.configure do |config|
  config.include SubprocessHelper
end
