module Widget
  # @api private
  class Matcher
    # Matches within a delta.
    #
    # @api public
    def of(delta)
    end

    def reset
    end

    class Step
      def advance
      end
    end
  end

  # @api private
  module Hidden
    # @api public
    class Shown
      def show
      end
    end
  end

  # @private
  class Legacy
    # @api public
    def run
    end
  end
end
