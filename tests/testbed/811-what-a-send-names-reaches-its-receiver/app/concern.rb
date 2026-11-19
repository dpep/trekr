module Concern
  def self.included(base)
    base.class_eval <<~RUBY
      def go(k) = Dial.public_send(k)
    RUBY
  end
end
