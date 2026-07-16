# Full parse+generate sweep vs gem json, mixed-workload alternating harness.
# Usage: ruby script/bench_sweep.rb <project_root> [block_seconds] [rounds]
require "json"
root = ARGV[0] || File.expand_path("..", __dir__)
block = (ARGV[1] || "0.3").to_f
rounds = (ARGV[2] || "5").to_i
$LOAD_PATH.unshift File.join(root, "lib")
require "nosj"
RubyVM::YJIT.enable if defined?(RubyVM::YJIT)

puts "ruby #{RUBY_VERSION} json #{JSON::VERSION} yjit=#{defined?(RubyVM::YJIT) && RubyVM::YJIT.enabled?} arch=#{RUBY_PLATFORM}"

files = Dir[File.join(root, "benchmark", "*.json")].sort
data = files.to_h { |f| [File.basename(f, ".json"), File.read(f)] }
objs = data.transform_values { |d| JSON.parse(d) }

# Sanity: byte parity before timing anything.
data.each do |n, d|
  raise "parse parity #{n}" unless NOSJ.parse(d) == JSON.parse(d)
  raise "gen parity #{n}" unless NOSJ.generate(objs[n]) == JSON.generate(objs[n])
end
puts "parity: OK (#{data.size} files, parse+generate)"

gt = Hash.new { |h, k| h[k] = {gem: [0.0, 0], spin: [0.0, 0]} }
pt = Hash.new { |h, k| h[k] = {gem: [0.0, 0], spin: [0.0, 0]} }
srand(29)
rounds.times do
  data.each_key do |name|
    jobs = [[gt, :gem, -> { JSON.generate(objs[name]) }], [gt, :spin, -> { NOSJ.generate(objs[name]) }],
      [pt, :gem, -> { JSON.parse(data[name]) }], [pt, :spin, -> { NOSJ.parse(data[name]) }]]
    jobs.shuffle.each do |tot, which, fn|
      GC.start
      t0 = Process.clock_gettime(Process::CLOCK_MONOTONIC)
      n = 0
      while Process.clock_gettime(Process::CLOCK_MONOTONIC) - t0 < block
        fn.call
        n += 1
      end
      tot[name][which][0] += Process.clock_gettime(Process::CLOCK_MONOTONIC) - t0
      tot[name][which][1] += n
    end
  end
end

pw = gw = 0
data.each_key do |name|
  pg = pt[name][:gem][1] / pt[name][:gem][0]
  ps = pt[name][:spin][1] / pt[name][:spin][0]
  gg = gt[name][:gem][1] / gt[name][:gem][0]
  gs = gt[name][:spin][1] / gt[name][:spin][0]
  pw += 1 if ps > pg
  gw += 1 if gs > gg
  puts format("%-18s parse %5.2fx %-4s (gem %8.0f spin %8.0f i/s)   generate %5.2fx %-4s (gem %8.0f spin %8.0f i/s)",
    name, pg / ps, (ps > pg) ? "WIN" : "loss", pg, ps, gg / gs, (gs > gg) ? "WIN" : "loss", gg, gs)
end
puts "TOTAL: parse #{pw}/#{data.size} wins, generate #{gw}/#{data.size} wins"
