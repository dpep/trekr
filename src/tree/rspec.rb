# RSpec as a suite sees it once it has booted: what rspec-core and
# rspec-expectations do at runtime that no reading of their source can follow.
#
# trekr reads this only when the index holds rspec-core, and only for what it
# adds: its classes and modules are the gems', so it declares none of them,
# and a method the gems define themselves wins over the one here. What is left
# is the wiring — a method made by `define_method` on a name held in a
# variable, a module included through a variable, a return type no signature
# states (DEC-087).

module RSpec
  # `RSpec::Core::DSL.expose_example_group_alias` defines each of these on
  # RSpec's singleton class, for every `define_example_group_method` in
  # rspec-core's example_group.rb. Each hands its block to the
  # `ExampleGroup` class method of the same name.
  def self.describe(*args, &example_group_block)
  end

  def self.context(*args, &example_group_block)
  end

  def self.example_group(*args, &example_group_block)
  end

  def self.xdescribe(*args, &example_group_block)
  end

  def self.xcontext(*args, &example_group_block)
  end

  def self.fdescribe(*args, &example_group_block)
  end

  def self.fcontext(*args, &example_group_block)
  end

  # `RSpec::Core::SharedExampleGroup::TopLevelDSL.expose_globally!`.
  def self.shared_examples(name, *args, &block)
  end

  def self.shared_context(name, *args, &block)
  end

  def self.shared_examples_for(name, *args, &block)
  end

  module Core
    # `Configuration#configure_mock_framework` and then
    # `#configure_expectation_framework`, run before the first group: the
    # default frameworks, included in that order.
    class ExampleGroup
      include RSpec::Core::MockingAdapters::RSpec
      include RSpec::Matchers
      # rspec-expectations' dsl.rb: `RSpec.configure { |c| c.extend self }`,
      # so `matcher :name do … end` works in a group's body.
      extend RSpec::Matchers::DSL
    end

    module MemoizedHelpers
      sig { returns(RSpec::Expectations::ValueExpectationTarget) }
      def is_expected
      end
    end
  end

  module Matchers
    # `RSpec::Expectations::Syntax.enable_expect` defines this, and
    # `ExpectationTarget.for` picks the target by whether a block was given.
    sig { params(value: T.untyped, block: NilClass).returns(RSpec::Expectations::ValueExpectationTarget) }
    sig { params(block: T.proc.void).returns(RSpec::Expectations::BlockExpectationTarget) }
    def expect(value = nil, &block)
    end
  end
end
