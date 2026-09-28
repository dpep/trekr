module RSpec
  module Core
    module MemoizedHelpers
      def is_expected
      end
    end

    class ExampleGroup
      include MemoizedHelpers

      def self.idempotently_define_singleton_method(name, &definition)
        (class << self; self; end).module_exec do
          define_method(name, &definition)
        end
      end

      def self.define_example_method(name, extra_options = {})
        idempotently_define_singleton_method(name) do |*all_args, &block|
        end
      end

      def self.define_example_group_method(name, metadata = {})
        idempotently_define_singleton_method(name) do |*args, &example_group_block|
          RSpec::Core::DSL.expose_example_group_alias(name)
        end
      end

      # The real one raises for a group-only name, and otherwise hands on
      # to `super`: RSpec::Matchers' own.
      def method_missing(name, *args)
        super(name, *args)
      end

      define_example_method :it
      define_example_group_method :describe
    end
  end
end
