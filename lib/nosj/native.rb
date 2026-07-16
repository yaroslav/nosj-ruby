# frozen_string_literal: true

# Precompiled platform gems ship one extension per Ruby minor under
# lib/nosj/<major.minor>/; the source gem compiles straight to
# lib/nosj/nosj.<dlext>.
begin
  ruby_version = RUBY_VERSION[/\d+\.\d+/]
  require_relative "#{ruby_version}/nosj"
rescue LoadError
  require_relative "nosj"
end
