module Extras
  def extra; end
end

class Parent
  class Helper
    def ph; end
  end
end

class Child < Parent
  def self.setup
    Helper.class_eval do
      include Extras
      def child_added; end
    end
  end

  def self.use
    Helper.new.ph
    Helper.new.child_added
  end
end
