# Demo server: rackup
require_relative "lib/widget"

run ->(env) { [200, {}, ["ok"]] }
