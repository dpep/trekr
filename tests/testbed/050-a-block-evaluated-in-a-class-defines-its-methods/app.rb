module Host
end

module Syntax
  def self.enable(host = ::Host)
    host.module_exec do
      def greet
      end
    end
  end
end

class Target
end

Target.class_eval do
  def aim
  end
end

class Visitor
  include Host

  def visit
    greet
    Target.new.aim
  end
end
