# frozen_string_literal: true

# Rails-mode encoder shoot-out: ActiveSupport::JSON.encode through the
# stock encoder, Oj's Rails mode (Oj.optimize_rails), and nosj's Rails
# mode (nosj/rails), on benchmark-ips.
#
# Usage:
#   bundle exec rake bench:rails
#
# Oj.optimize_rails and nosj/rails patch globally and cannot be undone,
# so the contenders cannot share a process. Every workload is ONE
# ips job listing all contenders, held across invocations
# (`x.hold!` measures the first pending report per invocation and
# loads the rest). The parent byte-compares the contenders'
# outputs, then runs the script once per
# contender in report order; the last pass prints the full `compare!`
# table for every workload. As everywhere else, the trusted numbers
# come from a PGO build (`rake bench:rails` compiles first).

require "json"
require "tmpdir"

CONTENDERS = %w[stock nosj oj].freeze
CHILD_ENV = "NOSJ_RAILS_CONTENDER"
LINEUP_ENV = "NOSJ_RAILS_LINEUP"
HOLD_DIR_ENV = "NOSJ_RAILS_HOLD_DIR"

def workloads
  require "bigdecimal"
  twitter = JSON.parse(File.read(File.expand_path("twitter.json", __dir__)))
  small = {"id" => 1, "name" => "ada", "tags" => ["x", "y"],
           "score" => 99.5, "active" => true}
  rich = {"time" => Time.at(0).utc, "date" => Date.new(2026, 7, 16),
          "dec" => BigDecimal("1.5"), "html" => "<script>&</script>",
          "floats" => [1.5, Float::NAN], "sym" => :ok}
  # A typical index endpoint: rows of records with a timestamp each.
  records = Array.new(100) do |i|
    {"id" => i, "email" => "user#{i}@example.com", "score" => i * 1.5,
     "admin" => i.zero?, "created_at" => Time.at(i * 3600).utc}
  end
  # Escape-heavy: user-generated content full of HTML.
  html_heavy = {"comments" => Array.new(50) do |i|
    {"id" => i, "body" => "<p>Comment #{i} says x < y && y > z</p>"}
  end}
  {
    "encode twitter (570 KB)" => -> { ActiveSupport::JSON.encode(twitter) },
    "encode 100 records+Time" => -> { ActiveSupport::JSON.encode(records) },
    "encode html-heavy" => -> { ActiveSupport::JSON.encode(html_heavy) },
    "encode Time/Date/BigDecimal" => -> { ActiveSupport::JSON.encode(rich) },
    "encode small hash" => -> { ActiveSupport::JSON.encode(small) },
    "small hash to_json" => -> { small.to_json }
  }
end

def load_contender(name)
  require "active_support"
  require "active_support/json"
  case name
  when "oj"
    require "oj"
    Oj.optimize_rails
  when "nosj"
    require_relative "../lib/nosj/rails"
  end
end

if (contender = ENV[CHILD_ENV])
  begin
    load_contender(contender)
  rescue LoadError => e
    warn "#{contender} unavailable: #{e.class}"
    exit
  end

  if ARGV[0] == "--correctness"
    workloads.each_value { |blk| puts blk.call }
    exit
  end

  # One held job per workload, every contender in the lineup as a
  # report. hold! measures only the first report without held results,
  # which is exactly this invocation's contender because the parent
  # runs the lineup in report order; the other blocks never execute
  # here. The final invocation has every result and compare! prints
  # the shoot-out.
  require "benchmark/ips"
  RubyVM::YJIT.enable
  lineup = ENV.fetch(LINEUP_ENV).split(",")
  hold_dir = ENV.fetch(HOLD_DIR_ENV)
  workloads.each do |label, blk|
    Benchmark.ips do |x|
      x.config(warmup: 1, time: 3)
      lineup.each { |c| x.report("#{c}: #{label}", &blk) }
      x.hold! File.join(hold_dir, label.gsub(/\W+/, "_"))
      x.compare! if contender == lineup.last
    end
  end
  exit
end

# Parent: correctness gate, then one pass per contender.
def run_child(contender, *args, env: {})
  cmd = [RbConfig.ruby, "-I", File.expand_path("../lib", __dir__), __FILE__, *args]
  IO.popen(env.merge(CHILD_ENV => contender), cmd, err: [:child, :out], &:read).tap do
    abort "#{contender} child failed" unless $?.success?
  end
end

puts "== correctness (every contender must produce stock's exact bytes)"
outputs = CONTENDERS.to_h { |c| [c, run_child(c, "--correctness")] }
available = CONTENDERS.select { |c| !outputs[c].include?("unavailable") }
available.each do |c|
  next if outputs[c] == outputs["stock"]
  abort "#{c} DIVERGES from the stock encoder:\n#{outputs[c]}"
end
skipped = CONTENDERS - available
puts skipped.empty? ? "ok" : "ok (skipped: #{skipped.join(", ")})"

Dir.mktmpdir("nosj-rails-bench") do |hold_dir|
  env = {LINEUP_ENV => available.join(","), HOLD_DIR_ENV => hold_dir}
  available.each do |c|
    puts "\n== measuring #{c}"
    puts run_child(c, env: env)
  end
end
