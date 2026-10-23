# frozen_string_literal: true

source "https://rubygems.org"

gemspec
gem "rake", "~> 13.0"
gem "rake-compiler"

group :benchmark do
  gem "benchmark-ips"
  gem "benchmark-memory"
  gem "oj"
  gem "yajl-ruby"
  gem "rapidjson"
  gem "fast_jsonparser"
end

group :test do
  gem "rspec", "~> 3.0"

  gem "multi_json"

  # Pinnable for CI's rails-compat matrix (e.g. "~> 7.1.0"); unpinned,
  # the newest release is what the main matrix exercises.
  rails_pin = ENV.fetch("NOSJ_ACTIVESUPPORT_VERSION", nil)
  gem "activesupport", *[rails_pin].compact
  gem "actionpack", *[rails_pin].compact
end

# Pinnable for CI's json-compat and rails-compat matrices (e.g. "< 3"):
# the drop-in follows whichever json is installed.
json_pin = ENV.fetch("NOSJ_JSON_VERSION", nil)
gem "json", json_pin if json_pin

group :development do
  # 1.57 is the first standard whose rubocop accepts json 3.
  gem "standard", "~> 1.57"
  gem "rbs", "~> 4.0"
  gem "yard", "~> 0.9"
  gem "lefthook", "~> 2.1"
end
