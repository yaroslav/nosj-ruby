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
  # Pure Ruby; the adapter spec exercises NOSJ::MultiJsonAdapter.
  gem "multi_json"
end

group :development do
  gem "standard", "~> 1.3"
  gem "rbs", "~> 4.0"
  gem "yard", "~> 0.9"
  gem "lefthook", "~> 2.1"
end
