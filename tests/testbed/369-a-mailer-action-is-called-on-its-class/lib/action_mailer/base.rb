module ActionMailer
  class MessageDelivery
    def initialize(mailer_class, action, *args)
      @mailer_class = mailer_class
      @action = action
      @args = args
    end
  end

  class Base
    class << self
      private

      def method_missing(method_name, ...)
        if action_methods.include?(method_name.name)
          MessageDelivery.new(self, method_name, ...)
        else
          super
        end
      end

      def respond_to_missing?(method, include_all = false)
        action_methods.include?(method.name) || super
      end
    end
  end
end
