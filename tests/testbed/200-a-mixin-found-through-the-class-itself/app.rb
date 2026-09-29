module Widget
  module Helpers
    def shout
    end
  end
end

module Widget
  module Parser
    class Base
    end

    class Widget < Base
      include ::Widget
      include Widget::Helpers

      def run
        shout
      end
    end

    class Fancy < Widget
    end
  end
end
