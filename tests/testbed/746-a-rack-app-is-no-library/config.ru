require_relative "lib/report"

run ->(env) { [200, {}, [Report.new.render]] }
