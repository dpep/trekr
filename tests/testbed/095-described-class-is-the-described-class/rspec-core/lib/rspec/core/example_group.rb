module RSpec
  module Core
    class ExampleGroup
      def self.described_class
      end

      def described_class
      end

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
        end
      end

      define_example_method :it
      define_example_group_method :describe
    end
  end
end
