module Macros
  def self.included(base)
    base.extend(ClassMethods)
  end

  module ClassMethods
    def add_helper(name)
      class_eval <<~RUBY, __FILE__, __LINE__ + 1
        def #{name}_helper
          run_#{name}
        end
      RUBY
    end

    def add_flags(*names)
      names.each do |name|
        class_eval "def #{name}?; end"
      end
    end
  end
end
