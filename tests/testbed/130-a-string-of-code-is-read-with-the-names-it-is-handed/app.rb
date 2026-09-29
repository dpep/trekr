module Macros
  def self.included(base)
    base.extend(ClassMethods)
  end

  module ClassMethods
    def add_helper(name)
      class_eval <<~RUBY
        def #{name}_helper
          run_#{name}
        end
      RUBY
    end
  end
end

class Widget
  include Macros
  add_helper :color

  def run_color
  end

  def use
    color_helper
  end
end

class Loops
  %w[alpha beta].each do |n|
    class_eval <<~RUBY
      def #{n}_x
        helper_#{n}
      end
    RUBY
  end

  def helper_alpha
  end

  def helper_beta
  end
end

class Maker
  def self.make(kind)
    class_eval "def go_#{kind}; run_#{kind}; end"
  end

  make :fast

  def run_fast
  end
end
