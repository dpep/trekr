class Reader
  def run(text)
    CStr.new(text).peek(1).upcase
    CStr.open(text)
  end
end
