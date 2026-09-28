RSpec::Matchers.define :have_widget do |expected|
  match { |actual| actual == expected }
end

RSpec::Matchers.define_negated_matcher :exclude, :include
