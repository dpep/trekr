module Macros
  def self.included(base)
    base.extend(ClassMethods)
  end

  module ClassMethods
    def add_helper
      class_eval <<~RUBY
        def helper_made
          :ok
        end
      RUBY
    end

    def add_reader(name)
      define_method(name) { name }
    end

    def add_orphan
      class_eval "def orphan_made; end"
    end

    def add_flags(*names)
      names.each do |name|
        class_eval "def #{name}?; end"
      end
    end
  end
end

class Widget
  include Macros
  add_helper
  add_reader :color
  add_flags :active, :hidden

  def use
    helper_made
  end
end

class Bystander
  include Macros
end

class Module
  def my_macro
    module_eval "def macro_#{name.downcase}; end"
  end
end

class Gadget
  my_macro
end

orphan_made
