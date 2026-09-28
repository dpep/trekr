class Widget
  class_attribute :registry, instance_writer: false
  thread_mattr_accessor :current

  def run
    self.registry = {}
    registry
    current
    Widget.current
  end
end
