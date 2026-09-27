class Gizmo
  define_method(:spin) { |times| times }

  define_method("turn") do
    wobble
  end

  def wobble
    "wobbled"
  end

  define_method(:twirl, instance_method(:wobble))

  def go
    spin(1)
    turn
    twirl
  end
end
