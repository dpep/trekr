class Widget
  def self.action(name)
    define_method(name) { name }
  end

  def self.labelled(name)
    define_method("#{name}_label") { name }
  end

  action :spin
  labelled :size

  def run
    spin
    size_label
  end
end
