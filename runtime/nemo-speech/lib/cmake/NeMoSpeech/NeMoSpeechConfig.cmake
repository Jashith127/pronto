# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0

####### Expanded from @PACKAGE_INIT@ by configure_package_config_file() #######
####### Any changes to this file will be overwritten by the next CMake run ####
####### The input file was NeMoSpeechConfig.cmake.in                            ########

get_filename_component(PACKAGE_PREFIX_DIR "${CMAKE_CURRENT_LIST_DIR}/../../../" ABSOLUTE)

macro(set_and_check _var _file)
  set(${_var} "${_file}")
  if(NOT EXISTS "${_file}")
    message(FATAL_ERROR "File or directory ${_file} referenced by variable ${_var} does not exist !")
  endif()
endmacro()

macro(check_required_components _NAME)
  foreach(comp ${${_NAME}_FIND_COMPONENTS})
    if(NOT ${_NAME}_${comp}_FOUND)
      if(${_NAME}_FIND_REQUIRED_${comp})
        set(${_NAME}_FOUND FALSE)
      endif()
    endif()
  endforeach()
endmacro()

####################################################################################

set(NeMoSpeech_ASR_FOUND FALSE)
set(NeMoSpeech_Diarization_FOUND FALSE)
set(NeMoSpeech_NMT_FOUND FALSE)
set(NeMoSpeech_TTS_FOUND FALSE)

function(_nemo_speech_import _component _library)
    find_library(_nemo_speech_${_component}_library
        NAMES ${_library}
        PATHS "${PACKAGE_PREFIX_DIR}/lib"
              "${PACKAGE_PREFIX_DIR}/bin"
        NO_DEFAULT_PATH)
    if(_nemo_speech_${_component}_library)
        add_library(NeMoSpeech::${_component} UNKNOWN IMPORTED)
        set_target_properties(NeMoSpeech::${_component} PROPERTIES
            IMPORTED_LOCATION "${_nemo_speech_${_component}_library}"
            INTERFACE_INCLUDE_DIRECTORIES "${PACKAGE_PREFIX_DIR}/include")
        set(NeMoSpeech_${_component}_FOUND TRUE PARENT_SCOPE)
    endif()
endfunction()

if(ON)
    _nemo_speech_import(ASR nemo_speech_asr_c)
endif()
if(ON)
    if(NeMoSpeech_ASR_FOUND)
        add_library(NeMoSpeech::Diarization INTERFACE IMPORTED)
        set_target_properties(NeMoSpeech::Diarization PROPERTIES
            INTERFACE_LINK_LIBRARIES NeMoSpeech::ASR
            INTERFACE_INCLUDE_DIRECTORIES "${PACKAGE_PREFIX_DIR}/include")
        set(NeMoSpeech_Diarization_FOUND TRUE)
    else()
        _nemo_speech_import(Diarization nemo_speech_asr_c)
    endif()
endif()
if(ON)
    _nemo_speech_import(NMT nemo_speech_nmt_c)
endif()
if(ON)
    _nemo_speech_import(TTS nemo_speech_tts)
endif()

check_required_components(NeMoSpeech)
