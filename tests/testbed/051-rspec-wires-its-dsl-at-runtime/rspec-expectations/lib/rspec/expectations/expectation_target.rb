module RSpec
  module Expectations
    class ExpectationTarget
      module InstanceMethods
        def to(matcher = nil, message = nil, &block)
        end
      end

      include InstanceMethods

      def self.for(value, block)
      end
    end

    class BlockExpectationTarget < ExpectationTarget
      def to(matcher, message = nil, &block)
      end
    end
  end
end
