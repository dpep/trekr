module RSpec
  module Expectations
    module Syntax
      def self.enable_expect(syntax_host = ::RSpec::Matchers)
        syntax_host.module_exec do
          def expect(value = nil, &block)
            ::RSpec::Expectations::ExpectationTarget.for(value, block)
          end
        end
      end
    end
  end

  module Matchers
    def eq(expected)
    end

    def raise_error(error = nil)
    end
  end
end
