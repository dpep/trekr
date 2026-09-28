module RSpec
  module Core
    module Hooks
      def before(*args, &block)
      end
    end

    module MemoizedHelpers
      module ClassMethods
        def let(name, &block)
        end
      end
    end

    class ExampleGroup
      extend MemoizedHelpers::ClassMethods
      extend Hooks

      def self.include_context(name, *args, &block)
      end

      def self.it_behaves_like(name, *args, &block)
      end

      def self.it(*args, &block)
      end
    end
  end
end
