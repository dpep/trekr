class Probe
  def via_class_eval
    Target.class_eval { def in_class_eval; end }
    Target.new.in_class_eval
  end

  def via_module_exec
    Target.module_exec do
      def in_module_exec
        in_class_eval
      end
    end
    Target.new.in_module_exec
  end

  def via_path
    Outer::Inner.class_exec { def in_path; end }
    Outer::Inner.new.in_path
  end

  def via_instance_eval
    Target.instance_eval { def in_instance_eval; end }
    Target.in_instance_eval
  end

  def via_define_method
    Target.module_eval { define_method(:made_in_eval) {} }
    Target.new.made_in_eval
  end

  Target.class_eval do
    def in_body
    end
  end

  class_eval do
    def implicit_eval
    end
  end

  def run
    Target.new.in_body
    implicit_eval
  end
end

module Holder
  def self.patch
    Target.class_eval { def shadowed; end }
    Target.new.shadowed
  end
end

class Probe
  self.class_eval do
    def self_eval
    end
  end

  def again
    self_eval
  end
end
