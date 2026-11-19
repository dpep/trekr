module Joined
  def self.included(base)
    base.class_eval <<~RUBY.lines.map(&:strip).join(";")
      def go(k)
        public_send(k)
      end
    RUBY
  end

  def small_attrs = 1
end

class Joiner
  include Joined
end
