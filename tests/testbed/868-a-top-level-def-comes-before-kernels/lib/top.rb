def format(x)
  "f#{x}"
end

format(3)

def helper_top
  :top
end

module Runner
  def self.go
    helper_top
  end
end
