# Run a gem's own spec suite under the trace, so the gold set records what its
# specs and library really dispatch — a second gold set, from code nobody wrote
# for this evaluation.
#
#     script/gold_gem.sh ~/code/lib/ruby/some_gem    # copies it, then does this
#
# One exerciser for every gem with an RSpec suite: the suite already walks the
# paths worth walking. `TREKR_SPECS` narrows it (default `spec`); the project's
# `.rspec` still applies, as it would to `bundle exec rspec`.

require "rspec/core"

status = RSpec::Core::Runner.run(ENV.fetch("TREKR_SPECS", "spec").split)
# A red suite still dispatched everything it ran; say so rather than stop.
warn "exercise_rspec: the suite exited #{status}" unless status.zero?
