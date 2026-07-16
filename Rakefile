# frozen_string_literal: true

require "bundler/gem_tasks"
require "rspec/core/rake_task"

RSpec::Core::RakeTask.new(:spec)

require "standard/rake"

require "rb_sys/extensiontask"

task build: :compile

GEMSPEC = Gem::Specification.load("nosj.gemspec")

# Every compile is a PGO build when a profile exists: `rake compile` applies
# tmp/pgo/merged.profdata (single fast build; rustc tolerates a stale profile,
# it just optimizes less). Run `rake compile:pgo` to retrain after significant
# changes. An explicit RUSTFLAGS wins: script/pgo.sh drives its own
# instrument/use phases through this same task. target-cpu=native must ride
# along because a RUSTFLAGS env replaces .cargo/config.toml target flags.
PGO_PROFILE = File.expand_path("tmp/pgo/merged.profdata", __dir__)
if ENV["RUSTFLAGS"].nil? && File.exist?(PGO_PROFILE)
  ENV["RUSTFLAGS"] = "-C target-cpu=native -C profile-use=#{PGO_PROFILE}"
  puts "rake compile: applying PGO profile (#{PGO_PROFILE}); `rake compile:pgo` retrains"
end

# The task name must equal the cargo PACKAGE (nosj_native, because the
# parser crate owns "nosj" in the cargo graph), but the ARTIFACT is plain
# nosj.bundle (lib target + extconf target are "nosj"), so the binary
# lookup is overridden to match.
RbSys::ExtensionTask.new("nosj_native", GEMSPEC) do |ext|
  ext.ext_dir = "ext/nosj"
  ext.lib_dir = "lib/nosj"
  def ext.binary(_platf = platform)
    "nosj.#{RbConfig::CONFIG["DLEXT"]}"
  end
end

# Profile-guided build: instrument -> train on the benchmark corpus ->
# rebuild with the profile. This is the shipping configuration and the only
# build that benchmark numbers should be quoted from (worth 5-30% here).
# Plain `rake compile` stays fast for the dev loop.
namespace :compile do
  desc "Compile with profile-guided optimization (the shipping build)"
  task :pgo do
    sh "./script/pgo.sh"
  end
end

# Benchmarks. Numbers worth quoting come from a PGO build and the
# alternating-block sweep (script/bench_sweep.rb, byte-parity gated), so
# that is what plain `rake bench` runs. Positional args pass through:
# rake "bench[0.2,3]" sets seconds per measurement block and rounds.
desc "PGO retrain, then the canonical parse+generate sweep vs gem json"
task :bench, [:block_s, :rounds] => "compile:pgo" do |_t, args|
  bench_sweep(args)
end

namespace :bench do
  desc "The sweep without retraining (reuses the last build and profile)"
  task :fast, [:block_s, :rounds] => :compile do |_t, args|
    bench_sweep(args)
  end

  desc "Multi-gem shoot-out on benchmark-ips: rake 'bench:ips[twitter,canada]'"
  task :ips, [:file] => :compile do |_t, args|
    ruby(*["benchmark/benchmark.rb", args[:file], *args.extras].compact)
  end
end

def bench_sweep(args)
  # bench_sweep.rb positionals: project root, block seconds, rounds.
  ruby(*["script/bench_sweep.rb", __dir__, args[:block_s], args[:rounds]].compact)
end

# Precompiled platform gems. script/ci/pgo-build-stage.sh stages one
# PGO-built, portable-codegen extension per Ruby minor under
# tmp/native-gem/<major.minor>/; this task packages every staged binary
# into pkg/nosj-<version>-<platform>.gem with the multi-ABI loader
# (lib/nosj/native.rb) picking the right one at require time.
desc "Package staged native extensions into a platform gem"
task :"gem:native", [:platform] do |_t, args|
  platform = args[:platform] or raise "usage: rake 'gem:native[x86_64-linux]'"
  staged = Dir["tmp/native-gem/*/nosj.*"].sort
  raise "nothing staged under tmp/native-gem/" if staged.empty?

  spec = GEMSPEC.dup
  spec.platform = Gem::Platform.new(platform)
  # Platform gems carry binaries, not sources: no extension compile on
  # install, no build-time-only rb_sys dependency, no Rust files.
  spec.extensions = []
  spec.dependencies.reject! { |d| d.name == "rb_sys" }
  spec.files = Dir["lib/**/*.rb"] + Dir["sig/**/*.rbs"] +
    %w[README.md CHANGELOG.md LICENSE.txt NOTICE]

  minors = []
  staged.each do |src|
    minor = File.basename(File.dirname(src))
    minors << minor
    dest = File.join("lib/nosj", minor, File.basename(src))
    mkdir_p File.dirname(dest)
    cp src, dest
    spec.files << dest
  end
  # A precompiled gem supports exactly the Rubies it embeds.
  minors.sort_by! { |m| Gem::Version.new(m) }
  spec.required_ruby_version = [">= #{minors.first}", "< #{minors.last.next}.dev"]

  mkdir_p "pkg"
  package = Gem::Package.build(spec)
  mv package, "pkg/#{package}"
  puts "built pkg/#{package} (Ruby #{minors.join(", ")})"
end

task default: %i[compile spec]
