class Widget
  VERBS.each do |verb|
    class_eval <<-RUBY, __FILE__, __LINE__ + 1
      def #{verb}(path)
        path
      end
    RUBY
  end
end

class Gadget
  def self.accessor(name)
    define_method(name) { name }
  end
end

class Part < Gadget
end

class Plain
  def other
  end
end

Widget.new.fetch
Part.new.color
Plain.new.fetch
