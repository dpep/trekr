module RSpec
  module Matchers
    module DSL
      def alias_matcher(new_name, old_name, options = {}, &description_override)
      end

      def define_negated_matcher(negated_name, base_name, &description_override)
      end

      def define(name, &declarations)
      end
      alias_method :matcher, :define
    end

    extend DSL

    def include(*expected)
    end

    def method_missing(method, *args, &block)
    end
  end
end
