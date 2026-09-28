module RSpec
  module Core
    module Hooks
      def before(*args, &block)
      end
    end

    module MemoizedHelpers
      def is_expected
      end

      module ClassMethods
        def let(name, &block)
        end
      end
    end

    class ExampleGroup
      include MemoizedHelpers
      extend MemoizedHelpers::ClassMethods
      extend Hooks

      def described_class
      end
    end
  end
end
