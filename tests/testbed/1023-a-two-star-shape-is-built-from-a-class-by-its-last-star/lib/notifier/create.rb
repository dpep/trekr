module Notifier
  class Create
    class UserAssignment
      def initialize(x)
        @x = x
      end
    end

    class GroupAssignment < UserAssignment
    end

    class LonelyAssignment < UserAssignment
    end

    def initialize(target)
      @target = target
    end

    def call
      "#{self.class}::#{@target.class}Assignment".constantize.new(@target)
    end
  end
end
