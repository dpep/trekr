module Widget
  module Util
    module_function

    def frozen?(node)
    end

    private

    def uninterpolated?(node)
    end

    public

    def literal?(node)
    end
  end

  module Named
    private

    def compatible?(a)
    end
    module_function :compatible?
  end
end
